use std::path::{Path, PathBuf};

use crate::path_env::{LoginShellError, PathEnv};

struct PresetSpec {
    id: &'static str,
    name: &'static str,
    binary: &'static str,
    args: &'static [&'static str],
}

// Flags checked from each binary's --help. Claude Code and Cursor CLI on
// 2026-10-03. Codex CLI 0.160.0 and Pi 1.0.0 on 2026-10-05. The Codex
// folder-trust override in `launch_flags` was verified in a real pty on
// 2026-10-09 with Codex CLI 0.161.0 (docs/tasks-and-worktrees.md). The Lead
// flags in `lead_flags` were checked on 2026-10-15 with Claude Code 2.1.300,
// Codex CLI 0.162.1, Cursor CLI 2026.10.01, and Pi 1.1.0 (docs/lead-agent.md).
const PRESETS: &[PresetSpec] = &[
    PresetSpec {
        id: "claude",
        name: "Claude Code",
        binary: "claude",
        args: &["--dangerously-skip-permissions"],
    },
    PresetSpec {
        id: "codex",
        name: "Codex",
        binary: "codex",
        args: &["--dangerously-bypass-approvals-and-sandbox"],
    },
    PresetSpec {
        id: "cursor",
        name: "Cursor CLI",
        binary: "agent",
        args: &["--yolo", "--trust", "--sandbox", "disabled"],
    },
    PresetSpec {
        id: "pi",
        name: "Pi",
        binary: "pi",
        args: &["--approve"],
    },
];

/// One CLI the picker offers. `path` is the absolute binary on the
/// login-shell PATH, or `None` when it is not installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliPreset {
    pub id: String,
    pub name: String,
    pub binary: String,
    pub args: Vec<String>,
    pub path: Option<PathBuf>,
}

impl CliPreset {
    pub fn found(&self) -> bool {
        self.path.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliCatalog {
    pub presets: Vec<CliPreset>,
    pub error: Option<LoginShellError>,
}

impl CliCatalog {
    pub(crate) fn from_env(env: &PathEnv) -> Self {
        Self {
            presets: presets_from(env),
            error: env.error(),
        }
    }
}

/// Binary names the login-shell PATH must resolve at startup.
pub fn binaries() -> Vec<&'static str> {
    PRESETS.iter().map(|preset| preset.binary).collect()
}

/// The arguments a launch in `worktree` gets: the preset's flags plus any
/// that depend on the worktree path. Codex shows a blocking "Trust this
/// folder?" dialog in every fresh worktree; `-c` with an inline `projects`
/// table marks only that worktree trusted for this process, with no write to
/// `~/.codex/config.toml`. The dotted form `projects."<path>".trust_level`
/// is not honored by 0.161.0, so the inline table is required. The prompt is
/// appended later, so it stays the last argument.
pub(crate) fn launch_flags(preset: &CliPreset, worktree: &Path) -> Vec<String> {
    let mut args = preset.args.clone();
    if preset.id == "codex" {
        args.push("-c".to_string());
        args.push(codex_trust_override(worktree));
    }
    args
}

/// How one Lead start addresses its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Conversation {
    /// A new conversation. `id` is the one Shika chose for Claude Code, Pi,
    /// and Cursor. None when the CLI picks it (Codex) or Cursor's
    /// `create-chat` failed.
    Fresh { id: Option<String> },
    /// Continue the stored conversation `id`.
    Resume { id: String },
}

/// The arguments a Lead starts with: the preset's launch flags, the Lead
/// instructions as the CLI's system prompt, and the conversation id. The
/// instructions go on every start, fresh or resumed. A Lead gets no positional
/// prompt here; Cursor's fresh-start bootstrap is appended by the caller.
///
/// | CLI | Instructions | Conversation |
/// | --- | --- | --- |
/// | Claude Code | `--append-system-prompt <text>` | `--session-id <uuid>`, then `--resume <uuid>` |
/// | Pi | `--append-system-prompt <text>` | `--session-id <id>` both times |
/// | Codex | `-c developer_instructions="<text>"` | none; `resume <id>` (a subcommand, so first) |
/// | Cursor CLI | none (the bootstrap prompt) | `--resume <id>` |
///
/// Codex's `developer_instructions` adds a developer message next to its
/// built-in instructions; `additional_developer_instructions` is ignored as an
/// unrecognized key (observed against a synthetic Responses server on
/// 2026-10-15, Codex CLI 0.162.1).
pub(crate) fn lead_flags(
    preset: &CliPreset,
    worktree: &Path,
    instructions: &str,
    conversation: &Conversation,
) -> Vec<String> {
    let mut args = launch_flags(preset, worktree);
    let mut push = |flag: &str, value: &str| {
        args.push(flag.to_string());
        args.push(value.to_string());
    };
    match (preset.id.as_str(), conversation) {
        ("claude" | "pi", Conversation::Fresh { id }) => {
            if let Some(id) = id {
                push("--session-id", id);
            }
            push("--append-system-prompt", instructions);
        }
        ("claude", Conversation::Resume { id }) => {
            push("--resume", id);
            push("--append-system-prompt", instructions);
        }
        ("pi", Conversation::Resume { id }) => {
            push("--session-id", id);
            push("--append-system-prompt", instructions);
        }
        ("codex", conversation) => {
            push(
                "-c",
                &format!("developer_instructions={}", toml_basic_string(instructions)),
            );
            if let Conversation::Resume { id } = conversation {
                args.insert(0, "resume".to_string());
                args.push(id.clone());
            }
        }
        ("cursor", Conversation::Fresh { id: Some(id) } | Conversation::Resume { id }) => {
            push("--resume", id);
        }
        _ => {}
    }
    args
}

/// Whether `id` is safe to pass as a conversation id argument: it comes from
/// a CLI or a file the user can edit, and must never read as a flag.
pub(crate) fn valid_conversation_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

/// `projects={"<path>"={trust_level="trusted"}}`, the path as a TOML basic
/// string. Codex compares the key with its working directory as the OS
/// reports it, so a symlinked path is resolved first.
fn codex_trust_override(worktree: &Path) -> String {
    let path = std::fs::canonicalize(worktree).unwrap_or_else(|_| worktree.to_path_buf());
    format!(
        "projects={{{}={{trust_level=\"trusted\"}}}}",
        toml_basic_string(&path.to_string_lossy())
    )
}

fn toml_basic_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{8}' => out.push_str("\\b"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\u{c}' => out.push_str("\\f"),
            '\r' => out.push_str("\\r"),
            c if c.is_control() => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Agents New can start, in preset order: the binary is on PATH, and the
/// caller has not turned that id off.
pub fn picker_presets(presets: &[CliPreset], enabled: impl Fn(&str) -> bool) -> Vec<&CliPreset> {
    presets
        .iter()
        .filter(|preset| preset.found() && enabled(&preset.id))
        .collect()
}

pub(crate) fn presets_from(env: &PathEnv) -> Vec<CliPreset> {
    PRESETS
        .iter()
        .map(|spec| CliPreset {
            id: spec.id.to_string(),
            name: spec.name.to_string(),
            binary: spec.binary.to_string(),
            args: spec.args.iter().map(|arg| (*arg).to_string()).collect(),
            path: env.get(spec.binary).map(|path| path.to_path_buf()),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_keep_a_missing_binary_and_the_launch_args() {
        let env = PathEnv::from_lookup(
            "/tmp/bin".into(),
            None,
            &[
                ("claude", Some(PathBuf::from("/tmp/bin/claude"))),
                ("codex", Some(PathBuf::from("/tmp/bin/codex"))),
                ("agent", None),
                ("pi", Some(PathBuf::from("/tmp/bin/pi"))),
            ],
        );

        let presets = presets_from(&env);

        assert_eq!(presets.len(), 4);
        assert_eq!(presets[0].id, "claude");
        assert_eq!(presets[0].name, "Claude Code");
        assert_eq!(presets[0].binary, "claude");
        assert_eq!(presets[0].args, ["--dangerously-skip-permissions"]);
        assert_eq!(
            presets[0].path.as_deref(),
            Some(std::path::Path::new("/tmp/bin/claude"))
        );
        assert!(presets[0].found());
        assert_eq!(presets[1].id, "codex");
        assert_eq!(presets[1].name, "Codex");
        assert_eq!(presets[1].binary, "codex");
        assert_eq!(
            presets[1].args,
            ["--dangerously-bypass-approvals-and-sandbox"]
        );
        assert!(presets[1].found());
        assert_eq!(presets[2].id, "cursor");
        assert_eq!(presets[2].name, "Cursor CLI");
        assert_eq!(presets[2].binary, "agent");
        assert_eq!(
            presets[2].args,
            ["--yolo", "--trust", "--sandbox", "disabled"]
        );
        assert_eq!(presets[2].path, None);
        assert!(!presets[2].found());
        assert_eq!(presets[3].id, "pi");
        assert_eq!(presets[3].name, "Pi");
        assert_eq!(presets[3].binary, "pi");
        assert_eq!(presets[3].args, ["--approve"]);
        assert!(presets[3].found());
        assert_eq!(binaries(), ["claude", "codex", "agent", "pi"]);

        let offered = picker_presets(&presets, |id| id != "pi");
        assert_eq!(
            offered
                .iter()
                .map(|preset| preset.id.as_str())
                .collect::<Vec<_>>(),
            ["claude", "codex"]
        );
    }

    fn preset(id: &str) -> CliPreset {
        let env = PathEnv::from_lookup(String::new(), None, &[]);
        presets_from(&env)
            .into_iter()
            .find(|preset| preset.id == id)
            .unwrap()
    }

    #[test]
    fn only_codex_gets_a_worktree_trust_override() {
        let path = Path::new("/no/such/repo/.worktrees/shika-draft-1");
        assert_eq!(
            launch_flags(&preset("codex"), path),
            [
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                r#"projects={"/no/such/repo/.worktrees/shika-draft-1"={trust_level="trusted"}}"#,
            ]
        );
        for id in ["claude", "cursor", "pi"] {
            let preset = preset(id);
            assert_eq!(launch_flags(&preset, path), preset.args, "{id}");
        }
    }

    const INSTRUCTIONS: &str = "You lead.\nRun \"shika help\".";
    const WORKTREE: &str = "/no/such/repo/.worktrees/shika-lead";

    fn lead(id: &str, conversation: Conversation) -> Vec<String> {
        lead_flags(
            &preset(id),
            Path::new(WORKTREE),
            INSTRUCTIONS,
            &conversation,
        )
    }

    fn fresh(id: Option<&str>) -> Conversation {
        Conversation::Fresh {
            id: id.map(str::to_string),
        }
    }

    fn resume(id: &str) -> Conversation {
        Conversation::Resume { id: id.into() }
    }

    #[test]
    fn claude_leads_get_a_session_id_then_resume_it_with_the_instructions_each_time() {
        let flags = ["--dangerously-skip-permissions"];
        let tail = ["--append-system-prompt", INSTRUCTIONS];
        assert_eq!(
            lead("claude", fresh(Some("uuid-1"))),
            [flags[0], "--session-id", "uuid-1", tail[0], tail[1]]
        );
        assert_eq!(
            lead("claude", resume("uuid-1")),
            [flags[0], "--resume", "uuid-1", tail[0], tail[1]]
        );
        // No positional prompt: the Lead starts idle.
        assert_eq!(lead("claude", fresh(None)).last().unwrap(), INSTRUCTIONS);
    }

    #[test]
    fn pi_leads_use_the_same_session_id_flag_fresh_and_resumed() {
        let expected = [
            "--approve",
            "--session-id",
            "id-7",
            "--append-system-prompt",
            INSTRUCTIONS,
        ];
        assert_eq!(lead("pi", fresh(Some("id-7"))), expected);
        assert_eq!(lead("pi", resume("id-7")), expected);
    }

    #[test]
    fn codex_leads_append_developer_instructions_and_resume_with_a_subcommand() {
        let trust = codex_trust_override(Path::new(WORKTREE));
        let instructions = r#"developer_instructions="You lead.\nRun \"shika help\".""#;
        assert_eq!(
            lead("codex", fresh(None)),
            [
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                &trust,
                "-c",
                instructions,
            ]
        );
        // `resume` is a subcommand, so it comes first; the id is last.
        assert_eq!(
            lead("codex", resume("019f-abc")),
            [
                "resume",
                "--dangerously-bypass-approvals-and-sandbox",
                "-c",
                &trust,
                "-c",
                instructions,
                "019f-abc",
            ]
        );
    }

    #[test]
    fn cursor_leads_carry_only_a_chat_id_because_the_instructions_are_a_prompt() {
        let flags = ["--yolo", "--trust", "--sandbox", "disabled"];
        let with = |extra: &[&str]| {
            flags
                .iter()
                .chain(extra)
                .map(|arg| arg.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            lead("cursor", fresh(Some("chat-1"))),
            with(&["--resume", "chat-1"])
        );
        assert_eq!(lead("cursor", fresh(None)), with(&[]));
        assert_eq!(
            lead("cursor", resume("chat-1")),
            with(&["--resume", "chat-1"])
        );
    }

    #[test]
    fn conversation_ids_cannot_read_as_flags() {
        for ok in ["0198f2a4-7a3e-7b0c-9a54-3f1f1b1b1b1b", "a_b.c-1", "x"] {
            assert!(valid_conversation_id(ok), "{ok}");
        }
        for bad in ["", "-x", "--resume", "a b", "a\nb", "a/b", &"x".repeat(129)] {
            assert!(!valid_conversation_id(bad), "{bad:?}");
        }
    }

    #[test]
    fn the_trust_path_is_a_toml_basic_string() {
        let path = Path::new("/no/such/we ird.d\"q\\b\tx/.worktrees/w");
        assert_eq!(
            codex_trust_override(path),
            r#"projects={"/no/such/we ird.d\"q\\b\tx/.worktrees/w"={trust_level="trusted"}}"#
        );
        assert_eq!(toml_basic_string("a\u{1}b"), "\"a\\u0001b\"");
    }

    #[test]
    fn a_symlinked_worktree_is_resolved_before_trusting_it() {
        let dir = std::env::temp_dir().join(format!("shika-trust-{}", std::process::id()));
        let real = dir.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let expected = std::fs::canonicalize(&real).unwrap();
        let arg = codex_trust_override(&link);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(arg.contains(&*expected.to_string_lossy()), "{arg}");
        assert!(!arg.contains("link"), "{arg}");
    }

    #[test]
    fn a_login_shell_error_leaves_every_preset_unresolved() {
        let env = PathEnv::from_lookup(
            String::new(),
            Some(LoginShellError::Spawn),
            &[("claude", None), ("agent", None)],
        );
        let catalog = CliCatalog::from_env(&env);
        assert!(catalog.presets.iter().all(|preset| preset.path.is_none()));
        assert_eq!(catalog.error, Some(LoginShellError::Spawn));
    }
}
