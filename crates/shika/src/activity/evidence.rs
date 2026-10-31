//! `OutputEvidence`: meaningful-output timestamps from transcript changes,
//! excluding drafts, footers, and live status animation.
use std::time::Instant;

use super::chrome::{rule, screen_frame, working_row};
use crate::model::is_agent_output;

/// Screen-based evidence for Unknown's quiet fallback. Raw output timestamps
/// are only a sampling watermark, never evidence by themselves. Keep one helper
/// per agent terminal, not per visible tab or per turn.
#[derive(Default)]
pub struct OutputEvidence {
    preset: String,
    transcript: Option<String>,
    observed_output: Option<Instant>,
    meaningful_output: Option<Instant>,
}

impl OutputEvidence {
    /// Returns the stable last meaningful output timestamp. `lines` must be the
    /// live screen, not the scrolled viewport. The first sample establishes a
    /// baseline without crediting startup text. Drafts, footers and live status
    /// animation are excluded; code inside fences is real transcript content.
    ///
    /// `interaction` must be the latest geometry/focus/scroll interaction, NOT
    /// typing. Draft signatures already exclude typing, so applying ECHO to keys
    /// would hide genuine responses while the user types. Within ECHO of that
    /// interaction, changed content is suppressed but its baseline and byte
    /// watermark are still consumed: delayed identical redraws cannot qualify.
    pub fn observe(
        &mut self,
        preset: &str,
        lines: &[String],
        output: Option<Instant>,
        interaction: Option<Instant>,
        now: Instant,
    ) -> Option<Instant> {
        if self.preset != preset {
            self.preset = preset.to_owned();
            self.transcript = None;
            self.observed_output = None;
            self.meaningful_output = None;
        }
        let signature = transcript_signature(preset, lines);
        let changed = self
            .transcript
            .as_ref()
            .is_some_and(|previous| previous != &signature);
        // Always update, including snapshots with no new bytes or suppressed
        // geometry changes. A future/old timestamp cannot retroactively credit
        // an already observed screen change.
        self.transcript = Some(signature);
        if let Some(output) = output
            .filter(|t| *t <= now && self.observed_output.is_none_or(|previous| *t > previous))
        {
            self.observed_output = Some(output);
            let geometry_echo = !is_agent_output(interaction.filter(|t| *t <= now), output);
            if changed && !geometry_echo {
                self.meaningful_output = Some(output);
            }
        }
        self.meaningful_output
    }
}

fn transcript_signature(preset: &str, lines: &[String]) -> String {
    let rows: Vec<&str> = lines.iter().map(|s| s.trim()).collect();
    let mut fenced = false;
    let in_code: Vec<bool> = rows
        .iter()
        .map(|s| {
            let code = fenced;
            if s.starts_with("```") || s.starts_with("~~~") {
                fenced = !fenced;
            }
            code
        })
        .collect();
    // Evidence cares about the input boundary, not whether a prompt is a
    // permission selector. In particular a draft consisting of "Yes" remains
    // input and cannot manufacture output evidence.
    let editor_footer = rows.iter().rev().find(|s| !s.is_empty()).is_some_and(|s| {
        s.contains("context left")
            || s.starts_with("? for shortcuts")
            || (preset == "pi" && s.contains('%') && (s.contains("auto") || s.contains('/')))
    });
    let prompt = rows.iter().enumerate().rposition(|(i, s)| {
        let s = s.trim_start_matches('│').trim();
        let marker = match preset {
            "claude" => s.starts_with('❯'),
            "codex" => s.starts_with('›'),
            // `→` is the composer, including a draft of "Yes". Decision rows use
            // the same glyph; the last matching row is still the input boundary.
            "cursor" => s.starts_with('>') || s.starts_with('❯') || s.starts_with('→'),
            _ => false,
        };
        // An unfinished response fence must not swallow the real editor. Its
        // framing/footer takes precedence over transcript Markdown context.
        marker
            && (!in_code[i]
                || editor_footer
                || (rows[..i].iter().any(|s| screen_frame(preset, s))
                    && rows[i + 1..].iter().any(|s| screen_frame(preset, s))))
    });
    let end = if let Some(prompt) = prompt {
        let top = (0..prompt).rev().find(|&i| screen_frame(preset, rows[i]));
        let bottom = ((prompt + 1)..rows.len()).any(|i| screen_frame(preset, rows[i]));
        if bottom {
            top.unwrap_or(prompt)
        } else {
            prompt
        }
    } else if preset == "pi" {
        rows.iter()
            .enumerate()
            .rposition(|(i, s)| (!in_code[i] || editor_footer) && rule(s))
            .and_then(|bottom| {
                (0..bottom).rev().find(|&i| {
                    (!in_code[i] || editor_footer) && (rule(rows[i]) || rows[i].starts_with("── "))
                })
            })
            .unwrap_or(rows.len())
    } else {
        rows.len()
    };
    let mut signature = String::new();
    for (i, row) in rows[..end].iter().enumerate() {
        if !in_code[i]
            && (rule(row)
                || working_row(preset, row)
                || footer_row(row)
                || (preset == "pi" && row.starts_with("── ") && row.ends_with("───")))
        {
            continue;
        }
        // Ignore all whitespace rather than row boundaries. Both word wrapping
        // and hard wrapping long paths on resize then preserve the signature.
        // Whitespace-only transcript edits are deliberately not evidence.
        signature.extend(row.chars().filter(|c| !c.is_whitespace()));
    }
    signature
}

fn footer_row(row: &str) -> bool {
    let row = row.to_lowercase();
    row.starts_with("? for shortcuts")
        || row.starts_with("esc to ")
        || row.starts_with("enter to ")
        || row.starts_with("press enter to ")
        || row.starts_with("ctrl+c to stop")
        || row.starts_with("↑/↓ to scroll")
        || row.starts_with("showing detailed transcript")
}
