//! Notice copy: titles, release-note excerpts, progress lines, Sparkle's
//! no-update reasons, and plain error text.
use super::Offer;

pub fn available_title(version: &str) -> String {
    if version.is_empty() {
        "A new version is available".into()
    } else {
        format!("Shika {version} is available")
    }
}

pub fn available_body(notes: &str, critical: bool, offer: Option<&Offer>) -> String {
    let fallback = match offer {
        Some(Offer::Open(_)) => "Open it in the browser, or ignore it until the next check.",
        Some(Offer::Download) => {
            "Download it when you want. Ignore leaves it until the next check."
        }
        None => "Ignore leaves it until the next check.",
    };
    let body = note_excerpt(notes).unwrap_or_else(|| fallback.to_string());
    if critical {
        format!("This update is critical. {body}")
    } else {
        body
    }
}

/// The first readable line of embedded release notes, capped so the corner
/// card stays one sentence. Markup is dropped.
pub fn note_excerpt(notes: &str) -> Option<String> {
    let plain = strip_tags(notes);
    let line = plain.lines().map(str::trim).find(|line| !line.is_empty())?;
    let line = line
        .trim_start_matches('#')
        .trim()
        .trim_start_matches(['*', '-', '>'])
        .trim()
        .replace('`', "");
    let text = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        None
    } else {
        Some(limit_chars(&text, 140))
    }
}

/// A display version safe to put in a title. Empty and control characters go.
pub fn version_name(raw: &str) -> Option<String> {
    let text: String = raw
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .take(40)
        .collect();
    if text.is_empty() { None } else { Some(text) }
}

/// An http(s) link from an information-only update. Anything else stays closed.
pub fn browser_url(raw: &str) -> Option<String> {
    let url = raw.trim();
    if url.starts_with("https://") || url.starts_with("http://") {
        Some(url.to_string())
    } else {
        None
    }
}

pub fn download_percent(received: u64, expected: u64) -> Option<u8> {
    received
        .saturating_mul(100)
        .checked_div(expected)
        .map(|percent| percent.min(100) as u8)
}

pub fn extraction_percent(progress: f64) -> Option<u8> {
    if !progress.is_finite() || progress < 0.0 {
        None
    } else {
        Some((progress * 100.0).round().clamp(0.0, 100.0) as u8)
    }
}

/// Sparkle's no-update reason, as `SPUNoUpdateFoundReason`'s integer value.
pub fn no_update_message(reason: isize) -> &'static str {
    match reason {
        1 | 2 => "No newer version found.",
        3 => "This Mac is too old for the latest version.",
        4 => "This Mac is too new for the latest version.",
        5 => "This Mac cannot run the latest version.",
        _ => "No update found.",
    }
}

pub fn plain_error(text: &str) -> String {
    let text = text.replace(['\u{2014}', '\u{2013}'], "-").replace('!', "");
    limit_chars(&text.split_whitespace().collect::<Vec<_>>().join(" "), 280)
}

pub(super) fn version_title(verb: &str, version: &str, fallback: &str) -> String {
    if version.is_empty() {
        fallback.into()
    } else {
        format!("{verb} Shika {version}")
    }
}

pub(super) fn version_sentence(
    version: &str,
    fallback: &str,
    with: impl Fn(&str) -> String,
) -> String {
    if version.is_empty() {
        fallback.into()
    } else {
        with(version)
    }
}

pub(super) fn percent_line(percent: Option<u8>, word: &str, fallback: &str) -> String {
    match percent {
        Some(percent) => format!("{percent}% {word}."),
        None => fallback.into(),
    }
}

fn strip_tags(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

fn limit_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(3)).collect();
    format!("{cut}...")
}
