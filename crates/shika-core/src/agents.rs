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
// 2026-10-09 with Codex CLI 0.161.0 (docs/tasks-and-worktrees.md).
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
