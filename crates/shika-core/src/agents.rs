use std::path::PathBuf;

use crate::path_env::{LoginShellError, PathEnv};

struct PresetSpec {
    id: &'static str,
    name: &'static str,
    binary: &'static str,
    args: &'static [&'static str],
}

// Flags checked 2026-10-03 from each binary's --help.
const PRESETS: &[PresetSpec] = &[
    PresetSpec {
        id: "claude",
        name: "Claude Code",
        binary: "claude",
        args: &["--dangerously-skip-permissions"],
    },
    PresetSpec {
        id: "cursor",
        name: "Cursor CLI",
        binary: "agent",
        args: &["--yolo", "--trust", "--sandbox", "disabled"],
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
                ("agent", None),
            ],
        );

        let presets = presets_from(&env);

        assert_eq!(presets.len(), 2);
        assert_eq!(presets[0].id, "claude");
        assert_eq!(presets[0].name, "Claude Code");
        assert_eq!(presets[0].binary, "claude");
        assert_eq!(presets[0].args, ["--dangerously-skip-permissions"]);
        assert_eq!(
            presets[0].path.as_deref(),
            Some(std::path::Path::new("/tmp/bin/claude"))
        );
        assert!(presets[0].found());
        assert_eq!(presets[1].id, "cursor");
        assert_eq!(presets[1].name, "Cursor CLI");
        assert_eq!(presets[1].binary, "agent");
        assert_eq!(
            presets[1].args,
            ["--yolo", "--trust", "--sandbox", "disabled"]
        );
        assert_eq!(presets[1].path, None);
        assert!(!presets[1].found());
        assert_eq!(binaries(), ["claude", "agent"]);
    }

    #[test]
    fn a_login_shell_error_leaves_both_unresolved() {
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
