//! The plain text the `shika` command prints for a reply: one compact line
//! per task, with its report indented below.

use std::fmt::Write as _;

use super::protocol::{Reply, TaskInfo};

fn duration_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m{:02}s", secs / 60, secs % 60),
        _ => format!("{}h{:02}m", secs / 3600, secs % 3600 / 60),
    }
}

fn task_line(task: &TaskInfo) -> String {
    let mut line = format!(
        "{} {} {:?} cli={}",
        task.id,
        task.status.as_str(),
        task.title,
        task.cli
    );
    if !task.branch.is_empty() {
        let _ = write!(line, " branch={}", task.branch);
    }
    if let Some(stat) = task.diff_stat {
        let _ = write!(
            line,
            " diff={} files +{} -{}",
            stat.files, stat.added, stat.removed
        );
    }
    if let Some(secs) = task.elapsed_secs {
        let _ = write!(line, " elapsed={}", duration_text(secs));
    }
    if let Some(pr) = task.pr {
        let _ = write!(line, " pr=#{pr}");
    }
    if task.started_by_lead {
        line.push_str(" by-lead");
    }
    if !task.path.is_empty() {
        if task.path.contains(char::is_whitespace) {
            let _ = write!(line, " path={:?}", task.path);
        } else {
            let _ = write!(line, " path={}", task.path);
        }
    }
    line
}

/// The task line, then its report (if any) on indented lines.
fn task_text(task: &TaskInfo) -> String {
    let mut text = task_line(task);
    if let Some(report) = &task.report {
        text.push_str("\n  report:");
        for line in report.lines() {
            text.push_str("\n    ");
            text.push_str(line.trim_end());
        }
    }
    text
}

/// Plain, compact text for the model: one line per task. `--json` output is
/// just `serde_json` of the [`Reply`].
pub fn render_text(reply: &Reply) -> String {
    match reply {
        Reply::Help { text } => text.trim_end().to_string(),
        Reply::Tasks { tasks } if tasks.is_empty() => "No tasks.".to_string(),
        Reply::Tasks { tasks } => tasks.iter().map(task_text).collect::<Vec<_>>().join("\n"),
        Reply::Started { task } => format!("started {}", task_text(task)),
        Reply::Status { task } => task_text(task),
        Reply::Text { text } => text.trim_end().to_string(),
        Reply::Done { message } => message.clone(),
        Reply::Waited {
            events,
            still_working,
            timed_out,
        } => {
            let mut lines = Vec::new();
            if events.is_empty() {
                lines.push(if *timed_out {
                    "timed out: no task needs attention yet".to_string()
                } else {
                    "nothing to wait for".to_string()
                });
            }
            for event in events {
                lines.push(format!(
                    "{}: {}",
                    event.task.status.as_str(),
                    task_text(&event.task)
                ));
            }
            if !still_working.is_empty() {
                lines.push("still working:".to_string());
                lines.extend(
                    still_working
                        .iter()
                        .map(|task| format!("  {}", task_line(task))),
                );
            }
            lines.join("\n")
        }
        Reply::Memory { notes } if notes.is_empty() => "No notes.".to_string(),
        Reply::Memory { notes } => notes
            .iter()
            .enumerate()
            .map(|(index, note)| format!("{}. {}", index + 1, note.text))
            .collect::<Vec<_>>()
            .join("\n"),
        Reply::Refused { reason } => format!("refused: {reason}"),
        Reply::Error { message } => format!("error: {message}"),
    }
}
