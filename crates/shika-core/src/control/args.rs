//! The client's front end: whether this process is the `shika` client, and
//! parsing its arguments into a [`Command`] with a short usage on error.

use super::protocol::{Command, key_name};
use super::{CLI_IDS, DEFAULT_WAIT_SECS, MAX_KEYS, MAX_READ_LINES, Result};
use crate::publish::MAX_PR_BODY_CHARS;

/// Usage text for argument errors.
pub const USAGE: &str = "usage: shika [--json] <command>\n  help\n  tasks\n  new --cli <claude|codex|cursor|pi> [--base <branch>] <prompt...>\n  status <task>\n  wait [<task>...] [--timeout <seconds>]\n  read <task> [--lines <n>]\n  diff <task> [--stat]\n  send <task> [--no-enter] <text...>\n  key <task> <key>...   (enter escape up down left right tab space backspace a-z 0-9)\n  pr <task> [--title <text>] [--body <text>]\n  close <task>
  memory
  remember <text...>
  forget <n>
  report <text...>   (workers only)";

/// Whether `word` is a command the `shika` client handles, so `main` can run
/// as the client before starting GPUI. [`is_client_invocation`] adds the
/// leading `--json` and the Lead-terminal rule.
pub fn is_command(word: &str) -> bool {
    matches!(
        word,
        "help"
            | "tasks"
            | "new"
            | "status"
            | "wait"
            | "read"
            | "diff"
            | "send"
            | "key"
            | "pr"
            | "close"
            | "memory"
            | "remember"
            | "forget"
            | "report"
    )
}

/// Whether this process runs as the `shika` client instead of the app.
/// `args` are the process arguments without the program name; `lead_env` is
/// whether `SHIKA_SOCKET` or `SHIKA_TOKEN` is set, which only a Lead's
/// terminal has. In a Lead terminal the binary is always the client, so a bare
/// or mistyped `shika` is a usage error and never a second app on the real
/// data. Elsewhere it is the client only for a command word, optionally after
/// a leading `--json`, so `--data-dir`, `--diagnostics-file`, and a Finder
/// launch still start the app.
pub fn is_client_invocation(args: &[String], lead_env: bool) -> bool {
    if lead_env {
        return true;
    }
    let mut args = args.iter();
    match args.next() {
        Some(first) if first == "--json" => args.next().is_some_and(|word| is_command(word)),
        Some(first) => is_command(first),
        None => false,
    }
}

fn usage_error(message: &str) -> String {
    format!("{message}\n{USAGE}")
}

/// Parses the arguments after the program name. Returns the command and
/// whether `--json` was given. `--json` is accepted before the command word
/// and, except inside a `new` prompt, anywhere after it. Errors carry a short
/// usage string.
pub fn parse_args(args: &[String]) -> Result<(Command, bool), String> {
    let mut json = false;
    let mut rest = args;
    while let Some((first, tail)) = rest.split_first() {
        if first == "--json" {
            json = true;
            rest = tail;
        } else {
            break;
        }
    }
    let Some((word, rest)) = rest.split_first() else {
        return Err(usage_error("Missing command."));
    };
    let command = match word.as_str() {
        "help" | "tasks" | "memory" => parse_bare(word, rest, &mut json)?,
        "new" => parse_new(rest, &mut json)?,
        "status" => parse_status(rest, &mut json)?,
        "wait" => parse_wait(rest, &mut json)?,
        "read" => parse_read(rest, &mut json)?,
        "diff" => parse_diff(rest, &mut json)?,
        "send" => parse_send(rest, &mut json)?,
        "key" => parse_key(rest, &mut json)?,
        "report" => Command::Report {
            text: parse_text(word, rest, &mut json)?,
        },
        "remember" => Command::Remember {
            text: parse_text(word, rest, &mut json)?,
        },
        "forget" => parse_forget(rest, &mut json)?,
        "pr" => parse_pr(rest, &mut json)?,
        "close" => parse_close(rest, &mut json)?,
        other => return Err(usage_error(&format!("Unknown command {other}."))),
    };
    Ok((command, json))
}

/// `help`, `tasks`, and `memory`: no arguments besides `--json`.
fn parse_bare(word: &str, args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut extra = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else {
            extra.push(arg);
        }
    }
    if !extra.is_empty() {
        return Err(usage_error(&format!("{word} takes no arguments.")));
    }
    Ok(match word {
        "help" => Command::Help,
        "tasks" => Command::Tasks,
        _ => Command::Memory,
    })
}

fn parse_status(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else if arg.starts_with('-') {
            return Err(usage_error(&format!("Unknown option {arg}.")));
        } else {
            tasks.push(arg.clone());
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Status { task }),
        Err(_) => Err(usage_error("status takes exactly one task.")),
    }
}

/// `wait [<task>...] [--timeout <seconds>]`: no tasks means every live task
/// this Lead started.
fn parse_wait(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut timeout_secs = DEFAULT_WAIT_SECS;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => *json = true,
            "--timeout" => {
                timeout_secs = iter
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or_else(|| usage_error("--timeout needs a number of seconds."))?;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    Ok(Command::Wait {
        tasks,
        timeout_secs,
    })
}

fn parse_close(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else if arg.starts_with('-') {
            return Err(usage_error(&format!("Unknown option {arg}.")));
        } else {
            tasks.push(arg.clone());
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Close { task }),
        Err(_) => Err(usage_error("close takes exactly one task.")),
    }
}

/// `new` flags come first; the first plain word starts the prompt, and
/// everything from there on is the prompt, even words that look like flags.
/// `--` also starts the prompt.
fn parse_new(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut cli = None;
    let mut base = None;
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--cli" | "--base" => {
                index += 1;
                let value = args
                    .get(index)
                    .ok_or_else(|| usage_error(&format!("{arg} needs a value.")))?;
                if arg == "--cli" {
                    cli = Some(value.clone());
                } else {
                    base = Some(value.clone());
                }
            }
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let cli = cli.ok_or_else(|| usage_error("new needs --cli."))?;
    if !CLI_IDS.contains(&cli.as_str()) {
        return Err(usage_error(&format!(
            "Unknown CLI {cli}. Use one of {}.",
            CLI_IDS.join(", ")
        )));
    }
    let prompt = args[index.min(args.len())..].join(" ");
    if prompt.trim().is_empty() {
        return Err(usage_error("new needs a prompt."));
    }
    Ok(Command::New { cli, base, prompt })
}

/// `pr <task> [--title <text>] [--body <text>]`: each value is one argument,
/// so the Lead quotes it. The title is one line, as Create PR's field is; the
/// body may span lines. Neither may be blank or given twice.
fn parse_pr(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut title = None;
    let mut body = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => *json = true,
            "--title" | "--body" => {
                let value = iter
                    .next()
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| usage_error(&format!("{arg} needs text.")))?;
                let slot = if arg == "--title" {
                    &mut title
                } else {
                    &mut body
                };
                if slot.replace(value.clone()).is_some() {
                    return Err(usage_error(&format!("{arg} is given twice.")));
                }
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    if title
        .as_deref()
        .is_some_and(|title| title.contains(['\n', '\r', '\0']))
    {
        return Err(usage_error("--title must be one line."));
    }
    if body
        .as_deref()
        .is_some_and(|body| body.contains('\0') || body.chars().count() > MAX_PR_BODY_CHARS)
    {
        return Err(usage_error(&format!(
            "--body is text of at most {MAX_PR_BODY_CHARS} characters."
        )));
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Pr { task, title, body }),
        Err(_) => Err(usage_error("pr takes exactly one task.")),
    }
}

fn parse_read(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut lines = 0;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--json" => *json = true,
            "--lines" => {
                lines = iter
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or_else(|| usage_error("--lines needs a number."))?;
                if lines > MAX_READ_LINES {
                    return Err(usage_error(&format!(
                        "--lines is at most {MAX_READ_LINES}."
                    )));
                }
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Read { task, lines }),
        Err(_) => Err(usage_error("read takes exactly one task.")),
    }
}

fn parse_diff(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut tasks = Vec::new();
    let mut stat = false;
    for arg in args {
        match arg.as_str() {
            "--json" => *json = true,
            "--stat" => stat = true,
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => tasks.push(other.to_string()),
        }
    }
    match <[String; 1]>::try_from(tasks) {
        Ok([task]) => Ok(Command::Diff { task, stat }),
        Err(_) => Err(usage_error("diff takes exactly one task.")),
    }
}

/// `send <task> [--no-enter] <text...>`: like `new`, flags come first and the
/// first plain word starts the text. `--` also starts it.
fn parse_send(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut rest = args;
    let mut enter = true;
    let task = loop {
        let Some((first, tail)) = rest.split_first() else {
            return Err(usage_error("send needs a task."));
        };
        rest = tail;
        match first.as_str() {
            "--json" => *json = true,
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            other => break other.to_string(),
        }
    };
    let mut index = 0;
    while let Some(arg) = rest.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--no-enter" => enter = false,
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let text = rest[index.min(rest.len())..].join(" ");
    if text.trim().is_empty() {
        return Err(usage_error("send needs text."));
    }
    Ok(Command::Send { task, text, enter })
}

/// `report <text...>` and `remember <text...>`: flags come first and the first
/// plain word starts the text. `--` also starts it, for text that begins with
/// a dash.
fn parse_text(word: &str, args: &[String], json: &mut bool) -> Result<String, String> {
    let mut index = 0;
    while let Some(arg) = args.get(index) {
        match arg.as_str() {
            "--json" => *json = true,
            "--" => {
                index += 1;
                break;
            }
            other if other.starts_with('-') => {
                return Err(usage_error(&format!("Unknown option {other}.")));
            }
            _ => break,
        }
        index += 1;
    }
    let text = args[index.min(args.len())..].join(" ");
    if text.trim().is_empty() {
        return Err(usage_error(&format!("{word} needs text.")));
    }
    Ok(text)
}

/// `forget <n>`: one note number, 1 or more.
fn parse_forget(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut words = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else if arg.starts_with('-') {
            return Err(usage_error(&format!("Unknown option {arg}.")));
        } else {
            words.push(arg.as_str());
        }
    }
    match words.as_slice() {
        [word] => match word.parse::<usize>() {
            Ok(note) if note >= 1 => Ok(Command::Forget { note }),
            _ => Err(usage_error("forget needs a note number, 1 or more.")),
        },
        _ => Err(usage_error("forget takes exactly one note number.")),
    }
}

fn parse_key(args: &[String], json: &mut bool) -> Result<Command, String> {
    let mut words = Vec::new();
    for arg in args {
        if arg == "--json" {
            *json = true;
        } else {
            words.push(arg.as_str());
        }
    }
    let Some((task, keys)) = words.split_first() else {
        return Err(usage_error("key needs a task."));
    };
    if task.starts_with('-') {
        return Err(usage_error(&format!("Unknown option {task}.")));
    }
    if keys.is_empty() {
        return Err(usage_error("key needs at least one key."));
    }
    if keys.len() > MAX_KEYS {
        return Err(usage_error(&format!("key takes at most {MAX_KEYS} keys.")));
    }
    let keys = keys
        .iter()
        .map(|key| key_name(key).ok_or_else(|| usage_error(&format!("Unknown key {key}."))))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Command::Key {
        task: task.to_string(),
        keys,
    })
}
