//! Presentation of one saved snapshot. No I/O, timers or collection here.

use crate::{MONO, UI_FONT, appearance::Chrome, dialog_text};
use gpui::{
    App, AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, Rgba, StatefulInteractiveElement, StyleRefinement, Styled, Window, div,
    prelude::FluentBuilder, px,
};
use shika_core::{UsageCalendar, UsageCalendarDay, UsageReport, UsageTotals, format_tokens};
use std::sync::Arc;

/// The provider row, left to right, and what the chart counts. The totals
/// row is the control, so its order is the filter order.
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
            Self::All => "Combined",
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

    fn maximum<'a>(self, days: impl IntoIterator<Item = Option<&'a UsageCalendarDay>>) -> u64 {
        days.into_iter()
            .flatten()
            .map(|day| self.tokens(day.tokens))
            .max()
            .unwrap_or(0)
    }
}

/// The period the totals and the chart cover: this local week or month as
/// daily bars, or the 52-week calendar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UsageRange {
    #[default]
    Week,
    Month,
    Year,
}

impl UsageRange {
    pub const CHOICES: [Self; 3] = [Self::Week, Self::Month, Self::Year];

    pub fn label(self) -> &'static str {
        match self {
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Year => "Year",
        }
    }

    /// `w`, `m` or `y` in Settings.
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "w" => Some(Self::Week),
            "m" => Some(Self::Month),
            "y" => Some(Self::Year),
            _ => None,
        }
    }

    fn period(self) -> &'static str {
        match self {
            Self::Week => "this week",
            Self::Month => "this month",
            Self::Year => "past year",
        }
    }

    pub fn totals(self, calendar: &UsageCalendar) -> UsageTotals {
        match self {
            Self::Week => calendar.this_week,
            Self::Month => calendar.this_month,
            Self::Year => calendar.totals,
        }
    }

    /// Every slot the chart draws, in order. Days after today are None.
    fn days(self, calendar: &UsageCalendar) -> Vec<Option<&UsageCalendarDay>> {
        match self {
            Self::Week => calendar.week(),
            Self::Month => calendar.month(),
            Self::Year => calendar
                .weeks
                .iter()
                .flat_map(|week| week.days.iter().map(Option::as_ref))
                .collect(),
        }
    }

    /// The first recorded day through today.
    fn span(self, calendar: &UsageCalendar) -> String {
        let first = match self {
            Self::Year => Some(calendar.start.as_str()),
            _ => self
                .days(calendar)
                .into_iter()
                .flatten()
                .next()
                .map(|day| day.date.as_str()),
        };
        format!("{} - {}", first.unwrap_or(&calendar.end), calendar.end)
    }
}

/// Settings > Usage while Settings is open. The request identity fences a
/// read that finishes after Settings was closed and opened again.
pub struct UsageView {
    pub request: Arc<()>,
    pub report: Option<UsageReport>,
    pub range: UsageRange,
    pub filter: UsageFilter,
    pub chart: Option<Entity<UsageChart>>,
}

impl UsageView {
    pub fn new() -> Self {
        Self {
            request: Arc::new(()),
            report: None,
            range: UsageRange::default(),
            filter: UsageFilter::default(),
            chart: None,
        }
    }

    pub fn load(&mut self, report: UsageReport, chrome: &Chrome, cx: &mut App) {
        let (range, filter) = (self.range, self.filter);
        self.chart = report
            .calendar
            .as_ref()
            .map(|calendar| cx.new(|_| UsageChart::new(calendar.clone(), range, filter, chrome)));
        self.report = Some(report);
    }

    pub fn set_filter(&mut self, filter: UsageFilter, cx: &mut App) {
        self.filter = filter;
        self.reconfigure(cx);
    }

    pub fn set_range(&mut self, range: UsageRange, cx: &mut App) {
        self.range = range;
        self.reconfigure(cx);
    }

    pub fn set_chrome(&self, chrome: &Chrome, cx: &mut App) {
        if let Some(chart) = &self.chart {
            let style = GridStyle::from_chrome(chrome);
            chart.update(cx, |chart, cx| {
                if chart.configure(chart.range, chart.filter, style) {
                    cx.notify();
                }
            });
        }
    }

    fn reconfigure(&self, cx: &mut App) {
        if let Some(chart) = &self.chart {
            let (range, filter) = (self.range, self.filter);
            chart.update(cx, |chart, cx| {
                if chart.configure(range, filter, chart.style) {
                    cx.notify();
                }
            });
        }
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

/// Combined, Claude Code and Codex for the selected range. Each column is
/// the filter control: `select` gives it its click.
pub fn totals(
    calendar: &UsageCalendar,
    range: UsageRange,
    filter: UsageFilter,
    chrome: &Chrome,
    mut select: impl FnMut(UsageFilter, gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div>,
) -> gpui::Div {
    let totals = range.totals(calendar);
    let mut row = div().flex().gap(px(12.));
    for (index, provider) in UsageFilter::CHOICES.into_iter().enumerate() {
        let selected = provider == filter;
        let ink = if selected { chrome.ink_1 } else { chrome.ink_3 };
        let column = div()
            .id(("usage-total", index))
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(6.))
            .pt(px(4.))
            .pb(px(12.))
            .border_b_1()
            .border_color(if selected {
                chrome.focus
            } else {
                chrome.line_2
            })
            .cursor_pointer()
            .when(!selected, |d| d.hover(|s| s.border_color(chrome.ink_4)))
            .child(
                dialog_text(chrome)
                    .text_color(ink)
                    .font_weight(FontWeight::MEDIUM)
                    .child(provider.label()),
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
                            .child(compact_tokens(provider.tokens(totals))),
                    )
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(chrome.ink_3)
                            .child(range.period()),
                    ),
            );
        row = row.child(select(provider, column));
    }
    row
}

// GitHub's contribution graph: 10px rounded squares on a 13px pitch, a
// weekday gutter wide enough for "Wed", and month labels above. The grid
// sets the Usage section's width, and every range draws in the same box, so
// switching ranges never moves the rows around it.
const CELL: f32 = 10.;
const GAP: f32 = 3.;
const RADIUS: f32 = 2.;
const GUTTER: f32 = 24.;
const MONTH_ROW: f32 = 16.;
const MONTH_GAP: f32 = 4.;
const LABEL: f32 = 10.5;
const WEEKS: f32 = 52.;

// The chart has a definite size, so GPUI can reuse its rendered subtree
// across unrelated card/timer redraws. One entity lives only as long as the
// Settings snapshot; hover, range, filter and theme changes invalidate it.
const GRID_WIDTH: f32 = GUTTER + GAP + WEEKS * (CELL + GAP) - GAP;
const GRID_HEIGHT: f32 = MONTH_ROW + MONTH_GAP + 7. * (CELL + GAP) - GAP;
/// Bars stand on the same box, with their day labels underneath.
const BAR_AREA: f32 = GRID_HEIGHT - MONTH_GAP - MONTH_ROW;
const BAR_RADIUS: f32 = 3.;

/// The Usage section's content width: the calendar.
pub const WIDTH: f32 = GRID_WIDTH;

#[derive(Clone, Copy, PartialEq)]
struct GridStyle {
    colors: [Rgba; 5],
    label: Rgba,
    hover: Rgba,
    tooltip_background: Rgba,
    tooltip_ink: Rgba,
}

impl GridStyle {
    fn from_chrome(chrome: &Chrome) -> Self {
        Self {
            colors: chrome.usage_intensities(),
            label: chrome.ink_3,
            hover: chrome.focus,
            tooltip_background: chrome.toast_bg,
            tooltip_ink: chrome.toast_fg,
        }
    }
}

pub struct UsageChart {
    calendar: UsageCalendar,
    range: UsageRange,
    filter: UsageFilter,
    style: GridStyle,
}

impl UsageChart {
    fn new(
        calendar: UsageCalendar,
        range: UsageRange,
        filter: UsageFilter,
        chrome: &Chrome,
    ) -> Self {
        Self {
            calendar,
            range,
            filter,
            style: GridStyle::from_chrome(chrome),
        }
    }

    fn configure(&mut self, range: UsageRange, filter: UsageFilter, style: GridStyle) -> bool {
        if self.range == range && self.filter == filter && self.style == style {
            return false;
        }
        self.range = range;
        self.filter = filter;
        self.style = style;
        true
    }
}

impl Render for UsageChart {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        match self.range {
            UsageRange::Year => calendar_grid(&self.calendar, self.filter, self.style),
            UsageRange::Week => {
                const NAMES: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
                bar_chart(
                    &self.calendar.week(),
                    40.,
                    self.filter,
                    self.style,
                    |index| Some(NAMES[index].to_string()),
                )
            }
            // The 1st and every fifth day, enough to place a bar without
            // crowding 31 narrow columns.
            UsageRange::Month => bar_chart(
                &self.calendar.month(),
                14.,
                self.filter,
                self.style,
                |index| {
                    let day = index + 1;
                    (day == 1 || day % 5 == 0).then(|| day.to_string())
                },
            ),
        }
    }
}

/// One square of the calendar or the legend, without a border at rest.
fn cell(color: Rgba) -> gpui::Div {
    div()
        .size(px(CELL))
        .flex_none()
        .rounded(px(RADIUS))
        .bg(color)
}

fn tooltip(
    day: &UsageCalendarDay,
    style: GridStyle,
) -> impl Fn(&mut Window, &mut App) -> gpui::AnyView + 'static {
    let day = day.clone();
    move |_, cx| {
        cx.new(|_| UsageTip {
            day: day.clone(),
            background: style.tooltip_background,
            ink: style.tooltip_ink,
        })
        .into()
    }
}

fn calendar_grid(calendar: &UsageCalendar, filter: UsageFilter, style: GridStyle) -> gpui::Div {
    let maximum = filter.maximum(UsageRange::Year.days(calendar));
    let mut months = div()
        .flex()
        .gap(px(GAP))
        .h(px(MONTH_ROW))
        .ml(px(GUTTER + GAP));
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
                .text_size(px(LABEL))
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
                .h(px(MONTH_ROW))
                .flex_none()
                .when_some(week.month.clone(), |d, label| {
                    d.child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .w(px(40.))
                            .whitespace_nowrap()
                            .text_size(px(LABEL))
                            .line_height(px(MONTH_ROW))
                            .text_color(style.label)
                            .child(label),
                    )
                }),
        );
        let mut column = div().flex_none().flex().flex_col().gap(px(GAP));
        for (weekday, day) in week.days.iter().enumerate() {
            column = column.child(match day {
                None => div().size(px(CELL)).flex_none().into_any_element(),
                Some(day) => cell(style.colors[bucket(filter.tokens(day.tokens), maximum)])
                    .id(("usage-day", week_index * 7 + weekday))
                    .border_1()
                    .border_color(gpui::transparent_black())
                    .hover(|s| s.border_color(style.hover))
                    .tooltip(tooltip(day, style))
                    .into_any_element(),
            });
        }
        columns = columns.child(column);
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(MONTH_GAP))
        .child(months)
        .child(columns)
}

/// One column per day across the chart's width: a track in the empty level
/// and a bar in a strong one, scaled to the busiest day of the range. Days
/// after today have no track, as in the calendar.
fn bar_chart(
    days: &[Option<&UsageCalendarDay>],
    bar_width: f32,
    filter: UsageFilter,
    style: GridStyle,
    label: impl Fn(usize) -> Option<String>,
) -> gpui::Div {
    let maximum = filter.maximum(days.iter().copied());
    let mut bars = div().flex().h(px(BAR_AREA));
    let mut labels = div().flex().h(px(MONTH_ROW));
    for (index, day) in days.iter().enumerate() {
        let slot = div().flex_1().min_w_0().flex().justify_center();
        bars = bars.child(slot.h_full().when_some(*day, |slot, day| {
            let height = bar_height(filter.tokens(day.tokens), maximum);
            slot.child(
                div()
                    .id(("usage-bar", index))
                    .group("usage-bar")
                    .w(px(bar_width))
                    .h_full()
                    .flex()
                    .flex_col()
                    .justify_end()
                    .rounded(px(BAR_RADIUS))
                    .bg(style.colors[0])
                    .hover(|s| s.bg(style.colors[1]))
                    .tooltip(tooltip(day, style))
                    .when(height > 0., |d| {
                        d.child(
                            div()
                                .w_full()
                                .h(px(height))
                                .rounded(px(BAR_RADIUS))
                                .bg(style.colors[3])
                                .group_hover("usage-bar", |s| s.bg(style.colors[4])),
                        )
                    }),
            )
        }));
        labels = labels.child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .justify_center()
                .text_size(px(LABEL))
                .line_height(px(MONTH_ROW))
                .text_color(style.label)
                .whitespace_nowrap()
                .when_some(label(index), |d, text| d.child(text)),
        );
    }
    div()
        .flex_none()
        .flex()
        .flex_col()
        .gap(px(MONTH_GAP))
        .child(bars)
        .child(labels)
}

/// A nonzero day is never shorter than 2px, so it reads as more than zero.
fn bar_height(tokens: u64, maximum: u64) -> f32 {
    if tokens == 0 || maximum == 0 {
        0.
    } else {
        ((tokens as f64 / maximum as f64) as f32 * BAR_AREA).clamp(2., BAR_AREA)
    }
}

/// The chart, then the range's dates and busiest day, with the intensity
/// legend under the calendar.
pub fn chart(
    calendar: &UsageCalendar,
    range: UsageRange,
    filter: UsageFilter,
    chrome: &Chrome,
    entity: &Entity<UsageChart>,
) -> gpui::Div {
    let maximum = filter.maximum(range.days(calendar));
    let label = |text: &'static str| {
        div()
            .text_size(px(LABEL))
            .line_height(px(CELL))
            .text_color(chrome.ink_3)
            .child(text)
    };
    let caption = div()
        .flex()
        .min_w_0()
        .items_baseline()
        .gap(px(6.))
        .text_size(px(11.))
        .text_color(chrome.ink_3)
        .child(div().font_family(MONO).child(range.span(calendar)))
        .when(maximum > 0, |d| {
            d.child(div().text_color(chrome.ink_5).child("\u{b7}"))
                .child(div().font_family(MONO).child(compact_tokens(maximum)))
                .child("busiest day")
        });
    let legend = (range == UsageRange::Year).then(|| {
        let mut legend = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(GAP))
            .child(label("Less").mr(px(4.)));
        for color in chrome.usage_intensities() {
            legend = legend.child(cell(color));
        }
        legend.child(label("More").ml(px(4.)))
    });
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            entity.clone().cached(
                StyleRefinement::default()
                    .w(px(GRID_WIDTH))
                    .h(px(GRID_HEIGHT))
                    .flex_none(),
            ),
        )
        .child(
            div()
                .w(px(GRID_WIDTH))
                .h(px(16.))
                .flex()
                .justify_between()
                .items_center()
                .gap(px(12.))
                .child(caption)
                .children(legend),
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

    fn day(date: &str, label: &str, claude: u64, codex: u64) -> Option<UsageCalendarDay> {
        Some(UsageCalendarDay {
            date: date.into(),
            label: label.into(),
            tokens: UsageTotals { claude, codex },
        })
    }

    /// Two weeks ending Tuesday 2026-10-13. A small fixture: its month
    /// starts at the second week, so the 12th and 13th are the month's days.
    fn calendar() -> UsageCalendar {
        UsageCalendar {
            start: "2026-10-05".into(),
            end: "2026-10-13".into(),
            this_week: UsageTotals {
                claude: 100,
                codex: 900,
            },
            this_month: UsageTotals {
                claude: 100,
                codex: 900,
            },
            totals: UsageTotals {
                claude: 1100,
                codex: 900,
            },
            weeks: vec![
                UsageWeek {
                    month: None,
                    days: [
                        day("2026-10-05", "Monday", 1000, 0),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                    ],
                },
                UsageWeek {
                    month: None,
                    days: [
                        day("2026-10-12", "Monday", 100, 300),
                        day("2026-10-13", "Tuesday", 0, 600),
                        None,
                        None,
                        None,
                        None,
                        None,
                    ],
                },
            ],
            month_first: 7,
            month_len: 31,
        }
    }

    #[test]
    fn filters_follow_the_totals_row_and_rescale_only_the_chart() {
        let calendar = calendar();
        let tokens = calendar.weeks[1].days[0].as_ref().unwrap().tokens;
        let week = UsageRange::Week.days(&calendar);
        for (filter, value, max, expected) in [
            (UsageFilter::All, 400, 600, 3),
            (UsageFilter::Claude, 100, 100, 4),
            (UsageFilter::Codex, 300, 600, 2),
        ] {
            assert_eq!(filter.tokens(tokens), value);
            assert_eq!(filter.maximum(week.iter().copied()), max);
            assert_eq!(bucket(value, max), expected);
        }
        let labels = UsageFilter::CHOICES.map(UsageFilter::label);
        assert_eq!(labels, ["Combined", "Claude Code", "Codex"]);
        assert_eq!(UsageFilter::All.step(-1), UsageFilter::All);
        assert_eq!(UsageFilter::All.step(1), UsageFilter::Claude);
        assert_eq!(UsageFilter::Codex.step(1), UsageFilter::Codex);
        assert_eq!(UsageFilter::Codex.step(-1), UsageFilter::Claude);
    }

    #[test]
    fn ranges_choose_totals_days_span_and_busiest_day() {
        let calendar = calendar();
        assert_eq!(UsageRange::default(), UsageRange::Week);
        for (range, combined, slots, span, claude_max) in [
            (UsageRange::Week, 1000, 7, "2026-10-12 - 2026-10-13", 100),
            (UsageRange::Month, 1000, 31, "2026-10-12 - 2026-10-13", 100),
            (UsageRange::Year, 2000, 14, "2026-10-05 - 2026-10-13", 1000),
        ] {
            assert_eq!(range.totals(&calendar).combined(), combined);
            let days = range.days(&calendar);
            assert_eq!(days.len(), slots);
            assert_eq!(range.span(&calendar), span);
            assert_eq!(UsageFilter::Claude.maximum(days), claude_max);
        }
        for (key, range) in [
            ("w", UsageRange::Week),
            ("m", UsageRange::Month),
            ("y", UsageRange::Year),
        ] {
            assert_eq!(UsageRange::from_key(key), Some(range));
        }
        assert_eq!(UsageRange::from_key("h"), None);
    }

    #[test]
    fn bars_scale_to_the_busiest_day_and_keep_small_days_visible() {
        assert_eq!(bar_height(0, 100), 0.);
        assert_eq!(bar_height(5, 0), 0.);
        assert_eq!(bar_height(100, 100), BAR_AREA);
        assert_eq!(bar_height(50, 100), BAR_AREA / 2.);
        assert_eq!(bar_height(1, u64::MAX), 2.);
        assert_eq!(bar_height(u64::MAX, u64::MAX), BAR_AREA);
    }

    #[test]
    fn chart_cache_invalidates_only_for_range_filter_and_used_theme_tokens() {
        use crate::appearance::chrome_for;
        use shika_core::Appearance;
        use shika_terminal::catalog;

        let chrome = chrome_for(&Appearance::default(), &catalog::THEMES[0], false);
        let mut chart = UsageChart::new(calendar(), UsageRange::Week, UsageFilter::All, &chrome);
        let style = GridStyle::from_chrome(&chrome);
        let (week, year) = (UsageRange::Week, UsageRange::Year);
        assert!(!chart.configure(week, UsageFilter::All, style));
        assert!(chart.configure(week, UsageFilter::Claude, style));
        assert!(!chart.configure(week, UsageFilter::Claude, style));
        assert!(chart.configure(year, UsageFilter::Claude, style));
        assert!(!chart.configure(year, UsageFilter::Claude, style));

        // Every color read by the chart or its tooltip participates in the
        // cache key. Changing unrelated chrome must not wake this view.
        let mut unrelated = chrome;
        unrelated.working.dot.r = 1. - unrelated.working.dot.r;
        assert!(!chart.configure(
            year,
            UsageFilter::Claude,
            GridStyle::from_chrome(&unrelated)
        ));
        for index in 0..9 {
            let mut changed = style;
            let color = match index {
                0..=4 => &mut changed.colors[index],
                5 => &mut changed.label,
                6 => &mut changed.hover,
                7 => &mut changed.tooltip_background,
                _ => &mut changed.tooltip_ink,
            };
            color.r = if color.r < 0.5 { 1. } else { 0. };
            assert!(chart.configure(year, UsageFilter::Claude, changed));
            assert!(!chart.configure(year, UsageFilter::Claude, changed));
            assert!(chart.configure(year, UsageFilter::Claude, style));
        }
        assert_eq!(GRID_WIDTH, 700.);
        assert_eq!(GRID_HEIGHT, 108.);
        assert_eq!(BAR_AREA, 88.);
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
