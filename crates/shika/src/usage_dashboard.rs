//! Presentation of one saved snapshot. No I/O, timers or collection here.

use crate::{MONO, UI_FONT, appearance::Chrome, dialog_text};
use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, Rgba, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
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

// The calendar has a definite size, so GPUI can reuse its rendered subtree
// across unrelated card/timer redraws. One entity lives only as long as the
// dashboard snapshot; hover, filter and theme changes invalidate its cache.
const GRID_WIDTH: f32 = GUTTER + GAP + 52. * (CELL + GAP) - GAP;
const GRID_HEIGHT: f32 = 16. + 6. + 7. * (CELL + GAP) - GAP;

#[derive(Clone, Copy, PartialEq)]
struct GridStyle {
    colors: [Rgba; 5],
    label: Rgba,
    border: Rgba,
    hover: Rgba,
    tooltip_background: Rgba,
    tooltip_ink: Rgba,
}

impl GridStyle {
    fn from_chrome(chrome: &Chrome) -> Self {
        Self {
            colors: chrome.usage_intensities(),
            label: chrome.ink_3,
            border: chrome.line_control,
            hover: chrome.focus,
            tooltip_background: chrome.overlay,
            tooltip_ink: chrome.ink_1,
        }
    }
}

pub struct UsageGrid {
    calendar: UsageCalendar,
    filter: UsageFilter,
    style: GridStyle,
}

impl UsageGrid {
    pub fn new(calendar: UsageCalendar, filter: UsageFilter, chrome: &Chrome) -> Self {
        Self {
            calendar,
            filter,
            style: GridStyle::from_chrome(chrome),
        }
    }

    pub fn set_filter(&mut self, filter: UsageFilter, cx: &mut Context<Self>) {
        if self.configure(filter, self.style) {
            cx.notify();
        }
    }

    pub fn set_chrome(&mut self, chrome: &Chrome, cx: &mut Context<Self>) {
        if self.configure(self.filter, GridStyle::from_chrome(chrome)) {
            cx.notify();
        }
    }

    fn configure(&mut self, filter: UsageFilter, style: GridStyle) -> bool {
        if self.filter == filter && self.style == style {
            return false;
        }
        self.filter = filter;
        self.style = style;
        true
    }
}

impl Render for UsageGrid {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        calendar_grid(&self.calendar, self.filter, self.style)
    }
}

fn calendar_grid(calendar: &UsageCalendar, filter: UsageFilter, style: GridStyle) -> gpui::Div {
    let maximum = filter.maximum(calendar);
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
                .text_color(style.label)
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
                            .text_color(style.label)
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
                    cell.id(("usage-day", week_index * 7 + weekday))
                        .bg(style.colors[bucket(filter.tokens(day.tokens), maximum)])
                        .border_1()
                        .border_color(style.border)
                        .hover(|s| s.border_color(style.hover))
                        .tooltip(move |_, cx| {
                            cx.new(|_| UsageTip {
                                day: tip_day.clone(),
                                background: style.tooltip_background,
                                ink: style.tooltip_ink,
                            })
                            .into()
                        })
                        .into_any_element()
                }
            });
        }
        columns = columns.child(column);
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(months)
        .child(columns)
}

pub fn heatmap(
    calendar: &UsageCalendar,
    filter: UsageFilter,
    chrome: &Chrome,
    grid: &Entity<UsageGrid>,
) -> gpui::Div {
    let maximum = filter.maximum(calendar);
    let colors = chrome.usage_intensities();
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
        .child(
            grid.clone().cached(
                StyleRefinement::default()
                    .w(px(GRID_WIDTH))
                    .h(px(GRID_HEIGHT))
                    .flex_none(),
            ),
        )
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
    background: Rgba,
    ink: Rgba,
}

impl Render for UsageTip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut tip = div()
            .px_2()
            .py_1()
            .rounded(px(6.))
            .bg(self.background)
            .text_color(self.ink)
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
    fn grid_cache_invalidates_only_for_filter_and_used_theme_tokens() {
        use crate::appearance::chrome_for;
        use shika_core::Appearance;
        use shika_terminal::catalog;

        let chrome = chrome_for(&Appearance::default(), &catalog::THEMES[0], false);
        let calendar = UsageCalendar {
            start: "2026-10-12".into(),
            end: "2026-10-13".into(),
            this_week: UsageTotals::default(),
            totals: UsageTotals::default(),
            weeks: Vec::new(),
        };
        let mut grid = UsageGrid::new(calendar, UsageFilter::All, &chrome);
        let style = GridStyle::from_chrome(&chrome);
        assert!(!grid.configure(UsageFilter::All, style));
        assert!(grid.configure(UsageFilter::Claude, style));
        assert!(!grid.configure(UsageFilter::Claude, style));

        // Every color read by the grid or its tooltip participates in the
        // cache key. Changing unrelated chrome must not wake this view.
        let mut unrelated = chrome;
        unrelated.working.dot.r = 1. - unrelated.working.dot.r;
        assert!(!grid.configure(UsageFilter::Claude, GridStyle::from_chrome(&unrelated)));
        for index in 0..10 {
            let mut changed = style;
            let color = match index {
                0..=4 => &mut changed.colors[index],
                5 => &mut changed.label,
                6 => &mut changed.border,
                7 => &mut changed.hover,
                8 => &mut changed.tooltip_background,
                _ => &mut changed.tooltip_ink,
            };
            color.r = if color.r < 0.5 { 1. } else { 0. };
            assert!(grid.configure(UsageFilter::Claude, changed));
            assert!(!grid.configure(UsageFilter::Claude, changed));
            assert!(grid.configure(UsageFilter::Claude, style));
        }
        assert_eq!(GRID_WIDTH, 760.);
        assert_eq!(GRID_HEIGHT, 118.);
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
