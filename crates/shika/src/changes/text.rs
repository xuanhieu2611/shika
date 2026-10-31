//! Line text and labels as the panel paints them: tabs, control characters,
//! display-width columns, and the file, size, and collapsed-row wording.

use super::TAB_WIDTH;
use shika_core::{Collapse, FileDiff, FileStatus};
use std::ops::Range;
use unicode_width::UnicodeWidthChar;

/// A character as the panel paints it. Control characters become their
/// Unicode control pictures, so a stray escape cannot reach the text system
/// and a `\r` in the middle of a line shows. C1 controls and the
/// bidirectional overrides become the replacement character, so text cannot
/// reorder itself on screen.
fn shown(ch: char) -> char {
    match ch {
        '\u{0}'..='\u{1f}' => char::from_u32(0x2400 + ch as u32).unwrap_or('\u{fffd}'),
        '\u{7f}' => '\u{2421}',
        '\u{80}'..='\u{9f}'
        | '\u{61c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}' => '\u{fffd}',
        ch => ch,
    }
}

/// A line's text without the `\r` of a CRLF ending, which every line of a
/// CRLF file has and which would only be noise.
fn without_cr(text: &str) -> &str {
    text.strip_suffix('\r').unwrap_or(text)
}

/// A character as painted, with the cells it takes: two for a wide
/// character (CJK, most emoji), none for a combining mark or another
/// zero-width character, one otherwise. Widths are per character, as a
/// terminal counts them, so an emoji sequence joined with U+200D counts each
/// of its emoji.
fn glyph(ch: char) -> (char, usize) {
    if matches!(ch, ' '..='~') {
        return (ch, 1);
    }
    let ch = shown(ch);
    (ch, UnicodeWidthChar::width(ch).unwrap_or(1))
}

/// The next tab stop after `column`.
fn tab_stop(column: usize) -> usize {
    (column / TAB_WIDTH + 1) * TAB_WIDTH
}

/// Columns a line takes as painted: tabs to the next `TAB_WIDTH` stop, other
/// characters their display width.
pub fn columns(text: &str) -> usize {
    without_cr(text).chars().fold(0, |column, ch| {
        if ch == '\t' {
            tab_stop(column)
        } else {
            column + glyph(ch).1
        }
    })
}

/// The part of a line painted from column `start`, at most `count` columns:
/// tabs expanded to spaces, a CRLF's `\r` dropped, control characters drawn
/// as symbols, and columns counted by display width. A wide character is
/// never split: when the left edge cuts it, its visible half is a space, and
/// one that starts in the last column is kept whole for the text area to
/// clip. Zero-width characters go with the character before them. Work stops
/// at the last column shown, so a very long line costs only the columns up to
/// the right edge, never the rest of the line.
pub fn visible_text(text: &str, start: usize, count: usize) -> String {
    let end = start.saturating_add(count);
    let mut out = String::with_capacity(count.min(text.len()));
    let mut column = 0;
    // Whether the last character that takes cells was painted, so a
    // combining mark after it is painted too, and one after a skipped
    // character is skipped with it.
    let mut painted = false;
    for ch in without_cr(text).chars() {
        if ch == '\t' {
            if column >= end {
                break;
            }
            let next = tab_stop(column);
            for _ in column.max(start)..next.min(end) {
                out.push(' ');
            }
            painted = next > start;
            column = next;
            continue;
        }
        let (ch, width) = glyph(ch);
        if width == 0 {
            if painted {
                out.push(ch);
            }
            continue;
        }
        if column >= end {
            break;
        }
        painted = column >= start;
        if painted {
            out.push(ch);
        } else {
            // A wide character that the left edge cuts shows its right half
            // as blank cells.
            for _ in start..(column + width).min(end) {
                out.push(' ');
            }
        }
        column += width;
    }
    out
}

/// A path or header made safe to paint on one line.
pub fn safe_label(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == '\t' { ' ' } else { shown(ch) })
        .collect()
}

/// `4,812 lines`, or `1 line`.
pub fn lines_label(n: usize) -> String {
    if n == 1 {
        "1 line".to_string()
    } else {
        format!("{} lines", thousands(n))
    }
}

/// A collapsed file's row, worded for the cap that collapsed it: its own
/// line or size cap, or the task's total budget, which collapses small files
/// too. Every one expands the same way.
pub fn collapsed_label(file: &FileDiff) -> String {
    let lines = lines_label(file.hidden_lines);
    match file.collapsed {
        Some(Collapse::Size(bytes)) if file.status == FileStatus::Untracked => {
            format!("Large file hidden - {}", size_label(bytes))
        }
        Some(Collapse::Size(bytes)) => format!("Large diff hidden - {}", size_label(bytes)),
        Some(Collapse::Budget) => format!("Diff hidden - {lines}"),
        Some(Collapse::Lines) | None => format!("Large diff hidden - {lines}"),
    }
}

/// A size in decimal units, as Finder shows it: `512 bytes`, `3.2 MB`.
pub fn size_label(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".to_string()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1000.;
    let mut unit = 0;
    // Round first, so 999,999 bytes reads 1.0 MB, not 1000.0 KB.
    while (value * 10.).round() >= 10_000. && unit + 1 < UNITS.len() {
        value /= 1000.;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// `4812` as `4,812`.
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// A file header's path: `old → new` for a rename. Returns the text and
/// the arrow's byte range, which paints dimmer.
pub fn file_label(file: &FileDiff) -> (String, Option<Range<usize>>) {
    let path = safe_label(&file.path);
    match &file.old_path {
        Some(old) => {
            let old = safe_label(old);
            let arrow = old.len() + 1..old.len() + 1 + '\u{2192}'.len_utf8();
            (format!("{old} \u{2192} {path}"), Some(arrow))
        }
        None => (path, None),
    }
}

/// Keep the basename prominent; renames retain both complete paths below it.
pub(super) fn card_labels(file: &FileDiff) -> (String, String) {
    let (directory, name) = file.path.rsplit_once('/').unwrap_or(("", &file.path));
    let directory = if file.old_path.is_some() {
        file_label(file).0
    } else {
        safe_label(directory)
    };
    (safe_label(name), directory)
}
