//! Row recognizers for each CLI's private chrome: prompts, frames, rules,
//! spinners, and working status rows.

pub(super) fn prompt_body<'a>(preset: &str, row: &'a str) -> Option<&'a str> {
    let row = row.trim_start_matches('│').trim();
    let marker = match preset {
        "claude" => '❯',
        "codex" => '›',
        "cursor" => {
            if row.starts_with('❯') {
                '❯'
            } else if row.starts_with('→') {
                '→'
            } else {
                '>'
            }
        }
        _ => return None,
    };
    let body = row.strip_prefix(marker)?.trim();
    // Cursor decision rows are `→ Run (once) (y)`, not the composer. A draft on
    // the composer can be "Yes"; that filter stays for the other glyphs.
    if preset == "cursor" && marker == '→' {
        if cursor_decision_option(body) {
            return None;
        }
        return Some(body);
    }
    let lower = body.to_lowercase();
    if (body.chars().next().is_some_and(|c| c.is_ascii_digit())
        && (body.contains(". ") || body.contains(") ")))
        || matches!(
            lower.as_str(),
            "yes" | "no" | "accept" | "decline" | "trust and continue"
        )
        || lower.starts_with("yes, ")
    {
        return None;
    }
    Some(body)
}

/// Box drawing, or Cursor's half-block prompt bar (`▄` above, `▀` below).
pub(super) fn screen_frame(preset: &str, s: &str) -> bool {
    rule(s) || (preset == "cursor" && half_block_bar(s))
}

fn half_block_bar(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c @ ('▄' | '▀')) => s.chars().count() >= 3 && chars.all(|next| next == c),
        _ => false,
    }
}

/// Selected approval rows look like `Run (once) (y)` or `Skip (esc or n)`.
fn cursor_decision_option(body: &str) -> bool {
    let Some((_, hint)) = body.rsplit_once(" (") else {
        return false;
    };
    (1..=18).contains(&hint.chars().count())
        && hint.ends_with(')')
        && hint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '+' | '-' | ')'))
}

/// Empty-input processing placeholder, painted on the right of the `→` row.
pub(super) fn cursor_processing_prompt(row: &str) -> bool {
    let Some(body) = row.trim_start_matches('│').trim().strip_prefix('→') else {
        return false;
    };
    let body = body.trim();
    body.eq_ignore_ascii_case("ctrl+c to stop") || body.to_lowercase().ends_with(" ctrl+c to stop")
}

pub(super) fn rule(s: &str) -> bool {
    s.chars().count() >= 3
        && s.chars().all(|c| {
            matches!(
                c,
                '─' | '━'
                    | '═'
                    | '╭'
                    | '╮'
                    | '╰'
                    | '╯'
                    | '┌'
                    | '┐'
                    | '└'
                    | '┘'
                    | '├'
                    | '┤'
                    | '┬'
                    | '┴'
                    | '┼'
            )
        })
}

fn spinner(c: char) -> bool {
    matches!(c, '\u{2801}'..='\u{28ff}' | '◐' | '◑' | '◒' | '◓')
}

pub(super) fn spinner_title(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(spinner)
        && chars.next() == Some(' ')
        && !chars.as_str().trim().is_empty()
}

pub(super) fn working_row(preset: &str, s: &str) -> bool {
    let lower = s.to_lowercase();
    let Some(first) = s.chars().next() else {
        return false;
    };
    let rest = s[first.len_utf8()..].trim();
    match preset {
        "claude" => {
            matches!(first, '*' | '·' | '✢' | '✳' | '✶' | '✻' | '✽' | '⏸' | '⏵')
                && (lower.contains("esc to interrupt")
                    || (rest.contains('…') && (rest.ends_with('…') || elapsed_suffix(rest)))
                    || (rest.starts_with("Waiting for ") && rest.ends_with(" to finish")))
        }
        "codex" => {
            let label = s
                .rsplit_once(" (")
                .map(|(label, _)| label)
                .unwrap_or(s)
                .trim_start_matches(['•', '◦'])
                .trim();
            let words: Vec<&str> = label.split_whitespace().collect();
            let activity = words
                .first()
                .is_some_and(|word| word.ends_with("ing") && word.chars().all(char::is_alphabetic));
            // An elapsed duration alone does not make prose into a live status.
            // Reduced-motion chrome has one activity label. Dynamic summaries
            // additionally need the explicit interrupt control, and completed
            // prose must never pin a card Working forever.
            let completed = words.iter().any(|word| {
                matches!(
                    word.to_lowercase()
                        .trim_matches(|c: char| !c.is_alphabetic()),
                    "completed"
                        | "complete"
                        | "finished"
                        | "done"
                        | "took"
                        | "was"
                        | "example"
                        | "result"
                )
            });
            activity
                && !completed
                && (words.len() == 1 || s.contains(" to interrupt)"))
                && !matches!(first, '›' | '■' | '✓' | '✗' | '>' | '`' | '"')
                && elapsed_suffix(s)
        }
        "cursor" => {
            lower == "ctrl+c to stop"
                || lower.starts_with("ctrl+c to stop ·")
                || cursor_status_line(s)
                || ((spinner(first) || matches!(first, '⬡' | '⬢'))
                    && rest
                        .split_whitespace()
                        .next()
                        .is_some_and(|s| s.ends_with("ing")))
        }
        "pi" => spinner(first) && (rest == "Working" || rest.starts_with("Working (")),
        _ => false,
    }
}

/// Require the CLI timer grammar, not an arbitrary parenthesized sentence.
/// Cursor CLI 2026.10 paints a one- or two-cell braille spinner, then a short
/// status word: Thinking, Summarizing, Working, or a progressive tool verb.
fn cursor_status_line(s: &str) -> bool {
    let mut chars = s.chars();
    if !chars.next().is_some_and(braille_cell) {
        return false;
    }
    if chars.as_str().chars().next().is_some_and(braille_cell) {
        chars.next();
    }
    let rest = chars.as_str().trim();
    // A long braille line is transcript text. Live status is a short label,
    // optionally followed by a path and a token count.
    if rest.split_whitespace().count() > 8 {
        return false;
    }
    let Some(word) = rest.split_whitespace().next() else {
        return false;
    };
    let word = word
        .trim_matches(|c: char| !c.is_alphabetic())
        .to_lowercase();
    // Cursor CLI 2026.10. Keep this list aligned with its status row, not with
    // every English word that ends in "ing".
    matches!(
        word.as_str(),
        "thinking"
            | "summarizing"
            | "working"
            | "composing"
            | "reconnecting"
            | "reading"
            | "grepping"
            | "searching"
            | "globbing"
            | "listing"
            | "deleting"
            | "editing"
            | "running"
            | "waiting"
            | "calling"
            | "exploring"
            | "updating"
            | "planning"
            | "writing"
            | "creating"
    )
}

fn braille_cell(c: char) -> bool {
    ('\u{2800}'..='\u{28ff}').contains(&c)
}

fn elapsed_suffix(s: &str) -> bool {
    let Some((_, suffix)) = s.rsplit_once(" (") else {
        return false;
    };
    let Some(suffix) = suffix.strip_suffix(')') else {
        return false;
    };
    let timer = suffix.split(" • ").next().unwrap_or("");
    if suffix
        .split_once(" • ")
        .is_some_and(|(_, hint)| !hint.ends_with(" to interrupt"))
    {
        return false;
    }
    let units: Vec<&str> = timer.split_whitespace().collect();
    !units.is_empty()
        && units.len() <= 3
        && units.last().is_some_and(|s| s.ends_with('s'))
        && units.iter().all(|s| {
            s.len() >= 2
                && matches!(s.as_bytes().last(), Some(b'h' | b'm' | b's'))
                && s[..s.len() - 1].bytes().all(|c| c.is_ascii_digit())
        })
}
