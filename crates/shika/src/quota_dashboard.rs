//! A quota snapshot, not a billing estimate or a live polling surface.
use crate::{MONO, appearance::Chrome};
use gpui::{FontWeight, ParentElement, Styled, div, px, relative};
use shika_core::{QuotaStatus, QuotaWindow, SubscriptionQuota};

pub(crate) fn rows(quotas: Option<&[SubscriptionQuota]>, now: i64, chrome: &Chrome) -> gpui::Div {
    let mut row = div().flex().gap(px(20.)).w_full();
    for (cli, name) in [("claude", "Claude Code"), ("codex", "Codex")] {
        let entries: Vec<_> = quotas
            .unwrap_or_default()
            .iter()
            .filter(|q| q.cli == cli)
            .collect();
        let mut column = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .gap(px(10.))
            .pb(px(12.))
            .border_b_1()
            .border_color(chrome.line_2);
        if entries.is_empty() {
            column = column.child(provider(name, None, quotas.is_none(), now, chrome));
        } else {
            for entry in &entries {
                let label = if entries.len() > 1 {
                    format!("{name} · configuration {}", entry.profile)
                } else {
                    name.to_string()
                };
                column = column.child(provider(&label, Some(entry), false, now, chrome));
            }
        }
        row = row.child(column);
    }
    row
}

fn provider(
    name: &str,
    quota: Option<&SubscriptionQuota>,
    loading: bool,
    now: i64,
    chrome: &Chrome,
) -> gpui::Div {
    let snapshot = quota.map(|q| q.snapshot).unwrap_or_default();
    let mut column = div().flex().flex_col().gap(px(8.)).child(
        div()
            .text_size(px(13.))
            .line_height(px(19.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(chrome.ink_1)
            .child(name.to_string()),
    );
    for (label, window) in [("5-hour", snapshot.five_hour), ("Weekly", snapshot.weekly)] {
        column = column.child(window_row(label, window, now, chrome));
    }
    let note = if loading {
        "Reading limits...".to_string()
    } else if let Some(quota) = quota {
        match quota.status {
            QuotaStatus::WaitingForActivity if quota.cli == "claude" => {
                "Updates after Claude Code activity.".into()
            }
            QuotaStatus::WaitingForActivity => "No quota observation yet.".into(),
            QuotaStatus::SignInRequired => "Sign in to Codex with ChatGPT.".into(),
            QuotaStatus::TimedOut => "Refresh timed out. Previous observations retained.".into(),
            QuotaStatus::Unavailable if quota.cli == "claude" => {
                "Limits unavailable. Existing status-line settings were left unchanged.".into()
            }
            QuotaStatus::Unavailable => {
                "Could not refresh limits. Previous observations retained.".into()
            }
            QuotaStatus::Observed => observation_note(snapshot.five_hour, snapshot.weekly, now),
        }
    } else if name.starts_with("Claude") {
        "Run Claude Code in Shika to read limits.".into()
    } else {
        "Codex limits are unavailable.".into()
    };
    column.child(
        div()
            .text_size(px(11.5))
            .line_height(px(16.))
            .text_color(chrome.ink_3)
            .child(note),
    )
}

fn window_row(label: &str, window: Option<QuotaWindow>, now: i64, chrome: &Chrome) -> gpui::Div {
    let active = window.filter(|window| !window.expired(now));
    let amount = active.map_or_else(
        || "Unavailable".into(),
        |w| format!("{}% used", percent(w.used_percent)),
    );
    let reset = active.map(|w| {
        format!(
            "Resets {} · in {}",
            reset_time(w.resets_at),
            duration(w.resets_at.saturating_sub(now))
        )
    });
    let mut row = div().flex().flex_col().gap(px(4.)).child(
        div()
            .flex()
            .justify_between()
            .items_center()
            .gap(px(8.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(chrome.ink_2)
                    .child(label.to_string()),
            )
            .child(
                div()
                    .font_family(MONO)
                    .text_size(px(11.5))
                    .text_color(chrome.ink_2)
                    .child(amount),
            ),
    );
    let mut track = div()
        .relative()
        .w_full()
        .h(px(3.))
        .rounded(px(3.))
        .bg(chrome.sunken);
    if let Some(window) = active {
        track = track.child(
            div()
                .h_full()
                .w(relative((window.used_percent / 100.) as f32))
                .rounded(px(3.))
                .bg(chrome.ink_3),
        );
    }
    row = row.child(track);
    row.child(
        div()
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(chrome.ink_3)
            .child(reset.unwrap_or_else(|| {
                if window.is_some() {
                    "Reset passed. Awaiting a new observation.".into()
                } else {
                    "No current observation.".into()
                }
            })),
    )
}

fn observation_note(five: Option<QuotaWindow>, weekly: Option<QuotaWindow>, now: i64) -> String {
    let observed = [five, weekly]
        .into_iter()
        .flatten()
        .filter(|w| !w.expired(now))
        .map(|w| w.observed_at)
        .min();
    match observed {
        Some(observed) if now.saturating_sub(observed) < 60 => {
            "Observed just now. Not a live balance.".into()
        }
        Some(observed) => format!(
            "Observed {} ago. Not a live balance.",
            duration(now.saturating_sub(observed))
        ),
        None => "Awaiting a new quota observation.".into(),
    }
}

fn reset_time(seconds: i64) -> String {
    use objc2_foundation::{NSDate, NSDateFormatter, ns_string};
    let formatter = NSDateFormatter::new();
    // Local date and the author's 12/24-hour convention, with no new date library.
    formatter.setLocalizedDateFormatFromTemplate(ns_string!("MMM d jm"));
    formatter
        .stringFromDate(&NSDate::dateWithTimeIntervalSince1970(seconds as f64))
        .to_string()
}

fn duration(seconds: i64) -> String {
    let minutes = seconds.saturating_add(59).max(0) / 60;
    match minutes {
        0..=59 => format!("{}m", minutes.max(1)),
        60..=1439 => format!("{}h {}m", minutes / 60, minutes % 60),
        _ => format!("{}d {}h", minutes / 1440, (minutes % 1440) / 60),
    }
}

fn percent(value: f64) -> String {
    if value.fract().abs() < 0.05 {
        format!("{value:.0}")
    } else {
        format!("{value:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn labels_preserve_missing_expired_and_observation_age() {
        assert_eq!(duration(7201), "2h 1m");
        assert_eq!(duration(604800), "7d 0h");
        assert_eq!(percent(12.5), "12.5");
        assert_eq!(
            observation_note(None, None, 100),
            "Awaiting a new quota observation."
        );
        let w = QuotaWindow {
            used_percent: 20.,
            resets_at: 2000,
            observed_at: 100,
        };
        assert!(observation_note(Some(w), None, 160).contains("1m ago"));
        assert!(!observation_note(Some(w), None, 2000).contains("ago"));
    }
}
