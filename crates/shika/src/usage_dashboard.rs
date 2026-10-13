//! Presentation of one saved snapshot. No I/O, timers or collection here.

use crate::{MONO, UI_FONT, appearance::Chrome, dialog_text};
use gpui::{
    AppContext, Context, FontWeight, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};
use shika_core::{UsageCalendar, UsageCalendarDay, UsageTotals, format_tokens};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageFilter {
    #[default]
    All,
    Claude,
    Codex,
}

impl UsageFilter {
    pub const CHOICES: [Self; 3] = [Self::All, Self::Claude, Self::Codex];

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "All",
            Self::Claude => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    pub fn step(self, direction: i32) -> Self {
        let at = Self::CHOICES
            .iter()
            .position(|choice| *choice == self)
            .unwrap_or(0);
        Self::CHOICES[(at as i32 + direction).clamp(0, 2) as usize]
    }

    pub fn tokens(self, totals: UsageTotals) -> u64 {
        match self {
            Self::All => totals.combined(),
            Self::Claude => totals.claude,
            Self::Codex => totals.codex,
        }
    }

    fn maximum(self, calendar: &UsageCalendar) -> u64 {
        calendar
            .weeks
            .iter()
            .flat_map(|week| week.days.iter().flatten())
            .map(|day| self.tokens(day.tokens))
            .max()
            .unwrap_or(0)
    }
}

/// Zero plus four equal intervals of the selected range's largest day.
/// u128 multiplication preserves boundaries even for saturated u64 totals.
fn bucket(tokens: u64, maximum: u64) -> usize {
    if tokens == 0 || maximum == 0 {
        0
    } else {
        (u128::from(tokens) * 4)
            .div_ceil(u128::from(maximum))
            .min(4) as usize
    }
}

fn compact_tokens(tokens: u64) -> String {
    if tokens < 1000 {
        return tokens.to_string();
    }
    let units = ["K", "M", "B", "T", "Q", "E"];
    let mut scale = 1000_u128;
    for (index, unit) in units.iter().enumerate() {
        let tenths = (u128::from(tokens) * 10 + scale / 2) / scale;
        if tenths < 10_000 || index == units.len() - 1 {
            return if tenths.is_multiple_of(10) {
                format!("{}{unit}", tenths / 10)
            } else {
                format!("{}.{digit}{unit}", tenths / 10, digit = tenths % 10)
            };
        }
        scale *= 1000;
    }
    unreachable!()
}

pub fn totals(calendar: &UsageCalendar, filter: UsageFilter, chrome: &Chrome) -> gpui::Div {
    let mut row = div().flex().gap(px(12.)).py(px(8.));
    for provider in [UsageFilter::Claude, UsageFilter::Codex, UsageFilter::All] {
        let selected = provider == filter;
        let ink = if selected { chrome.ink_1 } else { chrome.ink_3 };
        row = row.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(6.))
                .pb(px(12.))
                .border_b_1()
                .border_color(if selected {
                    chrome.focus
                } else {
                    chrome.line_2
                })
                .child(
                    dialog_text(chrome)
                        .text_color(ink)
                        .font_weight(FontWeight::MEDIUM)
                        .child(if provider == UsageFilter::All {
                            "Combined"
                        } else {
                            provider.label()
                        }),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(15.))
                                .line_height(px(20.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(ink)
                                .child(compact_tokens(provider.tokens(calendar.this_week))),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(chrome.ink_3)
                                .child("this week"),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_baseline()
                        .gap(px(8.))
                        .child(
                            div()
                                .font_family(MONO)
                                .text_size(px(14.))
                                .line_height(px(20.))
                                .text_color(ink)
                                .child(compact_tokens(provider.tokens(calendar.totals))),
                        )
                        .child(
                            div()
                                .text_size(px(11.5))
                                .text_color(chrome.ink_3)
                                .child("visible range"),
                        ),
                ),
        );
    }
    row
}

// 52 columns at 14px plus the weekday gutter fit the 880px dashboard even
// at the app's 960px minimum window width. Future cells occupy space only.
const CELL: f32 = 12.;
const GAP: f32 = 2.;
const GUTTER: f32 = 32.;

pub fn heatmap(calendar: &UsageCalendar, filter: UsageFilter, chrome: &Chrome) -> gpui::Div {
    let maximum = filter.maximum(calendar);
    let colors = chrome.usage_intensities();
    let mut months = div().flex().gap(px(GAP)).ml(px(GUTTER + GAP));
    let mut columns = div().flex().gap(px(GAP));
    let mut weekdays = div()
        .w(px(GUTTER))
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(GAP));
    for name in ["Mon", "", "Wed", "", "Fri", "", ""] {
        weekdays = weekdays.child(
            div()
                .h(px(CELL))
                .text_size(px(10.5))
                .line_height(px(CELL))
                .text_color(chrome.ink_3)
                .child(name),
        );
    }
    columns = columns.child(weekdays);
    for (week_index, week) in calendar.weeks.iter().enumerate() {
        months = months.child(
            div()
                .relative()
                .w(px(CELL))
                .h(px(16.))
                .flex_none()
                .when_some(week.month.clone(), |d, label| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(40.))
                            .text_size(px(10.5))
                            .text_color(chrome.ink_3)
                            .child(label),
                    )
                }),
        );
        let mut column = div().flex_none().flex().flex_col().gap(px(GAP));
        for (weekday, day) in week.days.iter().enumerate() {
            let cell = div().w(px(CELL)).h(px(CELL)).flex_none().rounded(px(4.));
            column = column.child(match day {
                None => cell.into_any_element(),
                Some(day) => {
                    let tip_day = day.clone();
                    let tip_chrome = *chrome;
                    cell.id(("usage-day", week_index * 7 + weekday))
                        .bg(colors[bucket(filter.tokens(day.tokens), maximum)])
                        .border_1()
                        .border_color(chrome.line_control)
                        .hover(|style| style.border_color(chrome.focus))
                        .tooltip(move |_, cx| {
                            cx.new(|_| UsageTip {
                                day: tip_day.clone(),
                                chrome: tip_chrome,
                            })
                            .into()
                        })
                        .into_any_element()
                }
            });
        }
        columns = columns.child(column);
    }
    let mut legend = div().flex().items_center().gap(px(GAP)).child(
        div()
            .text_size(px(10.5))
            .text_color(chrome.ink_3)
            .mr(px(6.))
            .child("Less"),
    );
    for color in colors {
        legend = legend.child(
            div()
                .size(px(CELL))
                .rounded(px(4.))
                .bg(color)
                .border_1()
                .border_color(chrome.line_control),
        );
    }
    legend = legend.child(
        div()
            .text_size(px(10.5))
            .text_color(chrome.ink_3)
            .ml(px(6.))
            .child(format!("More · {} max/day", compact_tokens(maximum))),
    );
    div()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(months)
        .child(columns)
        .child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .mt(px(6.))
                .child(
                    div()
                        .font_family(MONO)
                        .text_size(px(11.))
                        .text_color(chrome.ink_3)
                        .child(format!("{} - {}", calendar.start, calendar.end)),
                )
                .child(legend),
        )
}

struct UsageTip {
    day: UsageCalendarDay,
    chrome: Chrome,
}

impl Render for UsageTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut tip = div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(self.chrome.overlay)
            .text_color(self.chrome.ink_1)
            .text_size(px(12.))
            .font_family(UI_FONT)
            .flex()
            .flex_col()
            .gap(px(6.))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.day.label.clone()),
            );
        for (label, value) in [
            ("Claude Code", self.day.tokens.claude),
            ("Codex", self.day.tokens.codex),
            ("Total", self.day.tokens.combined()),
        ] {
            tip = tip.child(
                div()
                    .flex()
                    .justify_between()
                    .gap(px(20.))
                    .child(label)
                    .child(div().font_family(MONO).child(format_tokens(value))),
            );
        }
        tip
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shika_core::UsageWeek;

    #[test]
    fn buckets_have_exact_boundaries_and_handle_zero_and_overflow() {
        for (n, expected) in [
            (0, 0),
            (1, 1),
            (25, 1),
            (26, 2),
            (50, 2),
            (51, 3),
            (75, 3),
            (76, 4),
            (100, 4),
            (101, 4),
        ] {
            assert_eq!(bucket(n, 100), expected);
        }
        assert_eq!(bucket(0, 0), 0);
        assert_eq!(bucket(1, 1), 4);
        assert_eq!(bucket(u64::MAX, u64::MAX), 4);
        assert_eq!(bucket(u64::MAX / 4, u64::MAX), 1);
    }

    #[test]
    fn filters_select_provider_totals_and_rescale_only_the_heatmap() {
        let tokens = UsageTotals {
            claude: 100,
            codex: 300,
        };
        let other = UsageTotals {
            claude: 0,
            codex: 600,
        };
        let calendar = UsageCalendar {
            start: "2026-10-12".into(),
            end: "2026-10-13".into(),
            this_week: UsageTotals {
                claude: 100,
                codex: 900,
            },
            totals: UsageTotals {
                claude: 100,
                codex: 900,
            },
            weeks: vec![UsageWeek {
                month: Some("Oct".into()),
                days: [
                    Some(UsageCalendarDay {
                        date: "2026-10-12".into(),
                        label: "Monday".into(),
                        tokens,
                    }),
                    Some(UsageCalendarDay {
                        date: "2026-10-13".into(),
                        label: "Tuesday".into(),
                        tokens: other,
                    }),
                    None,
                    None,
                    None,
                    None,
                    None,
                ],
            }],
        };
        for (filter, value, max, expected) in [
            (UsageFilter::All, 400, 600, 3),
            (UsageFilter::Claude, 100, 100, 4),
            (UsageFilter::Codex, 300, 600, 2),
        ] {
            assert_eq!(filter.tokens(tokens), value);
            assert_eq!(filter.maximum(&calendar), max);
            assert_eq!(bucket(value, max), expected);
        }
        assert_eq!(UsageFilter::All.tokens(calendar.totals), 1000);
        assert_eq!(UsageFilter::Claude.tokens(calendar.totals), 100);
        assert_eq!(UsageFilter::Codex.tokens(calendar.totals), 900);
        assert_eq!(UsageFilter::All.step(-1), UsageFilter::All);
        assert_eq!(UsageFilter::All.step(1), UsageFilter::Claude);
        assert_eq!(UsageFilter::Codex.step(1), UsageFilter::Codex);
        assert_eq!(UsageFilter::Codex.step(-1), UsageFilter::Claude);
    }

    #[test]
    fn compact_numbers_round_and_promote_without_losing_small_counts() {
        for (value, text) in [
            (0, "0"),
            (999, "999"),
            (1000, "1K"),
            (1234, "1.2K"),
            (999_950, "1M"),
            (1_234_567, "1.2M"),
            (1_000_000_000, "1B"),
            (u64::MAX, "18.4E"),
        ] {
            assert_eq!(compact_tokens(value), text);
        }
    }
}
