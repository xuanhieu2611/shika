//! The shortcuts Settings can change. Everything else stays on its built-in
//! chord. A new chord replaces the previous one, including a default.

use std::collections::HashSet;

use gpui::Keystroke;
use shika_core::KeyOverrides;

/// One row in Settings, Keyboard.
pub struct Command {
    pub id: &'static str,
    pub label: &'static str,
    /// None starts unbound. The chord is GPUI's binding syntax (`cmd-r`).
    pub default: Option<&'static str>,
}

/// Hide or show changes, then go to agent 1 through 9, then go to tab 1
/// through 9. Earlier rows win when two saved chords collide.
pub static COMMANDS: &[Command] = &[
    Command {
        id: "toggleChanges",
        label: "Hide or show changes",
        default: Some("cmd-alt-b"),
    },
    Command {
        id: "selectAgent1",
        label: "Go to agent 1",
        default: None,
    },
    Command {
        id: "selectAgent2",
        label: "Go to agent 2",
        default: None,
    },
    Command {
        id: "selectAgent3",
        label: "Go to agent 3",
        default: None,
    },
    Command {
        id: "selectAgent4",
        label: "Go to agent 4",
        default: None,
    },
    Command {
        id: "selectAgent5",
        label: "Go to agent 5",
        default: None,
    },
    Command {
        id: "selectAgent6",
        label: "Go to agent 6",
        default: None,
    },
    Command {
        id: "selectAgent7",
        label: "Go to agent 7",
        default: None,
    },
    Command {
        id: "selectAgent8",
        label: "Go to agent 8",
        default: None,
    },
    Command {
        id: "selectAgent9",
        label: "Go to agent 9",
        default: None,
    },
    Command {
        id: "selectTab1",
        label: "Go to tab 1",
        default: Some("cmd-1"),
    },
    Command {
        id: "selectTab2",
        label: "Go to tab 2",
        default: Some("cmd-2"),
    },
    Command {
        id: "selectTab3",
        label: "Go to tab 3",
        default: Some("cmd-3"),
    },
    Command {
        id: "selectTab4",
        label: "Go to tab 4",
        default: Some("cmd-4"),
    },
    Command {
        id: "selectTab5",
        label: "Go to tab 5",
        default: Some("cmd-5"),
    },
    Command {
        id: "selectTab6",
        label: "Go to tab 6",
        default: Some("cmd-6"),
    },
    Command {
        id: "selectTab7",
        label: "Go to tab 7",
        default: Some("cmd-7"),
    },
    Command {
        id: "selectTab8",
        label: "Go to tab 8",
        default: Some("cmd-8"),
    },
    Command {
        id: "selectTab9",
        label: "Go to tab 9",
        default: Some("cmd-9"),
    },
];

/// What is bound right now. `chord` is GPUI's canonical form, or None.
pub struct Binding {
    pub id: &'static str,
    pub chord: Option<String>,
}

/// Chords Settings must not take. They belong to the rest of the app, or
/// to the terminal's copy and paste.
const RESERVED: &[(&str, &str)] = &[
    ("cmd-n", "New agent"),
    ("cmd-l", "New Lead"),
    ("cmd-t", "New terminal tab"),
    ("cmd-shift-p", "Create PR"),
    ("cmd-w", "Close terminal tab"),
    ("cmd-shift-w", "Close task"),
    ("cmd-shift-r", "Rename task"),
    ("cmd-]", "Next agent"),
    ("cmd-[", "Previous agent"),
    ("cmd-b", "Hide or show agent column"),
    ("cmd-q", "Quit"),
    ("cmd-,", "Settings"),
    ("cmd-h", "Hide Shika"),
    ("cmd-alt-h", "Hide others"),
    ("cmd-c", "Copy"),
    ("cmd-v", "Paste"),
];

/// The chord bound to `id`, as the user should read it (`⌘R`). None when
/// that row has no shortcut.
pub fn bound_label(overrides: &KeyOverrides, id: &str) -> Option<String> {
    bindings(overrides)
        .into_iter()
        .find(|binding| binding.id == id)
        .and_then(|binding| binding.chord)
        .map(|chord| display(&chord))
}

/// `selectAgent3` is slot 2. Anything else is None.
pub fn agent_slot(id: &str) -> Option<usize> {
    numbered_slot(id, "selectAgent")
}

/// `selectTab1` is slot 0, the pinned agent tab.
pub fn tab_slot(id: &str) -> Option<usize> {
    numbered_slot(id, "selectTab")
}

fn numbered_slot(id: &str, prefix: &str) -> Option<usize> {
    let n: usize = id.strip_prefix(prefix)?.parse().ok()?;
    (1..=9).contains(&n).then_some(n - 1)
}

/// Resolved shortcuts. An override beats a default. A chord can belong to
/// only one row; an explicit override beats another row's default.
pub fn bindings(overrides: &KeyOverrides) -> Vec<Binding> {
    let mut chords = vec![None; COMMANDS.len()];
    let mut taken = HashSet::new();
    for (index, command) in COMMANDS.iter().enumerate() {
        let Some(raw) = overrides.get(command.id) else {
            continue;
        };
        if raw.is_empty() {
            continue;
        }
        if let Some(chord) = user_chord(raw)
            && reserved_name(&chord).is_none()
            && taken.insert(chord.clone())
        {
            chords[index] = Some(chord);
        }
    }
    for (index, command) in COMMANDS.iter().enumerate() {
        if overrides.get(command.id).is_some() {
            continue;
        }
        if let Some(chord) = command.default.and_then(user_chord)
            && reserved_name(&chord).is_none()
            && taken.insert(chord.clone())
        {
            chords[index] = Some(chord);
        }
    }
    COMMANDS
        .iter()
        .zip(chords)
        .map(|(command, chord)| Binding {
            id: command.id,
            chord,
        })
        .collect()
}

/// Give `id` a chord, or clear it when `chord` is None. The previous owner
/// of that chord, if it is one of these rows, loses it.
pub fn assign(
    overrides: &KeyOverrides,
    id: &str,
    chord: Option<String>,
) -> Result<KeyOverrides, String> {
    if !COMMANDS.iter().any(|command| command.id == id) {
        return Err("That shortcut cannot be changed.".into());
    }
    let chord = match chord {
        None => None,
        Some(raw) => {
            let Some(chord) = user_chord(&raw) else {
                return Err("Use a Command shortcut.".into());
            };
            if let Some(name) = reserved_name(&chord) {
                return Err(format!("{} is {name}.", display(&chord)));
            }
            Some(chord)
        }
    };
    let mut effective = bindings(overrides);
    if let Some(chord) = &chord {
        for binding in &mut effective {
            if binding.id != id && binding.chord.as_ref() == Some(chord) {
                binding.chord = None;
            }
        }
    }
    for binding in &mut effective {
        if binding.id == id {
            binding.chord.clone_from(&chord);
        }
    }
    Ok(store(&effective))
}

/// The next key the user pressed while a row was recording.
pub fn chord_from_keystroke(stroke: &Keystroke) -> Result<String, String> {
    if !stroke.modifiers.platform || stroke.modifiers.control || stroke.modifiers.function {
        return Err("Use a Command shortcut.".into());
    }
    if !acceptable_key(&stroke.key) {
        return Err("Use a Command shortcut.".into());
    }
    let chord = stroke.unparse();
    if let Some(name) = reserved_name(&chord) {
        return Err(format!("{} is {name}.", display(&chord)));
    }
    Ok(chord)
}

/// `⌘R`, `⌥⌘B`, `⌘⇧W`. Option, then Command, then Shift, then the key.
pub fn display(chord: &str) -> String {
    let Ok(stroke) = Keystroke::parse(chord) else {
        return chord.to_string();
    };
    let mut out = String::new();
    if stroke.modifiers.alt {
        out.push('\u{2325}');
    }
    if stroke.modifiers.platform {
        out.push('\u{2318}');
    }
    if stroke.modifiers.shift {
        out.push('\u{21e7}');
    }
    out.push_str(&key_label(&stroke.key));
    out
}

fn store(effective: &[Binding]) -> KeyOverrides {
    let mut out = KeyOverrides::default();
    for binding in effective {
        let Some(command) = COMMANDS.iter().find(|command| command.id == binding.id) else {
            continue;
        };
        let default = command.default.and_then(user_chord);
        if binding.chord.as_deref() == default.as_deref() {
            continue;
        }
        match &binding.chord {
            Some(chord) => out.set(binding.id, chord.clone()),
            None => out.set(binding.id, ""),
        }
    }
    out
}

fn user_chord(raw: &str) -> Option<String> {
    let stroke = Keystroke::parse(raw).ok()?;
    if !stroke.modifiers.platform || stroke.modifiers.control || stroke.modifiers.function {
        return None;
    }
    if !acceptable_key(&stroke.key) {
        return None;
    }
    Some(stroke.unparse())
}

fn reserved_name(chord: &str) -> Option<&'static str> {
    let chord = user_chord(chord)?;
    RESERVED
        .iter()
        .find(|(raw, _)| user_chord(raw).as_deref() == Some(chord.as_str()))
        .map(|(_, name)| *name)
}

fn acceptable_key(key: &str) -> bool {
    match key {
        "up" | "down" | "left" | "right" | "tab" | "space" | "pageup" | "pagedown" | "home"
        | "end" | "delete" | "backspace" | "enter" | "escape" => true,
        key => {
            let mut chars = key.chars();
            let Some(ch) = chars.next() else {
                return false;
            };
            chars.next().is_none() && (ch.is_ascii_alphanumeric() || ch.is_ascii_punctuation())
        }
    }
}

fn key_label(key: &str) -> String {
    match key {
        "left" => "\u{2190}".into(),
        "right" => "\u{2192}".into(),
        "up" => "\u{2191}".into(),
        "down" => "\u{2193}".into(),
        "tab" => "\u{21e5}".into(),
        "escape" => "\u{238b}".into(),
        "backspace" => "\u{232b}".into(),
        "enter" => "\u{21a9}".into(),
        "space" => "Space".into(),
        key if key.len() == 1 => {
            let ch = key.chars().next().unwrap();
            if ch.is_ascii_lowercase() {
                ch.to_ascii_uppercase().to_string()
            } else {
                key.to_string()
            }
        }
        key => key.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::Modifiers;

    fn stroke(key: &str, modifiers: Modifiers) -> Keystroke {
        Keystroke {
            modifiers,
            key: key.into(),
            key_char: None,
        }
    }

    fn chord(raw: &str) -> String {
        user_chord(raw).unwrap()
    }

    fn bound(overrides: &KeyOverrides, id: &str) -> Option<String> {
        bindings(overrides)
            .into_iter()
            .find(|binding| binding.id == id)
            .and_then(|binding| binding.chord)
    }

    #[test]
    fn defaults_bind_changes_and_tabs_and_leave_agents_free() {
        let overrides = KeyOverrides::default();
        assert_eq!(
            bound(&overrides, "toggleChanges").as_deref(),
            Some(chord("cmd-alt-b")).as_deref()
        );
        assert_eq!(
            bound(&overrides, "selectTab1").as_deref(),
            Some(chord("cmd-1")).as_deref()
        );
        assert_eq!(
            bound(&overrides, "selectTab9").as_deref(),
            Some(chord("cmd-9")).as_deref()
        );
        assert_eq!(bound(&overrides, "selectAgent1"), None);
        assert_eq!(
            bound_label(&overrides, "toggleChanges").as_deref(),
            Some("\u{2325}\u{2318}B")
        );
        assert_eq!(
            bound_label(&overrides, "selectTab1").as_deref(),
            Some("\u{2318}1")
        );
    }

    #[test]
    fn a_new_changes_chord_replaces_the_old_one() {
        let next = assign(
            &KeyOverrides::default(),
            "toggleChanges",
            Some("cmd-r".into()),
        )
        .unwrap();
        assert_eq!(
            bound(&next, "toggleChanges").as_deref(),
            Some(chord("cmd-r")).as_deref()
        );
        assert!(
            bindings(&next)
                .iter()
                .all(|binding| { binding.chord.as_deref() != Some(chord("cmd-alt-b").as_str()) })
        );
        assert_eq!(
            bound_label(&next, "toggleChanges").as_deref(),
            Some("\u{2318}R")
        );
        // Putting the default back stores nothing.
        let restored = assign(&next, "toggleChanges", Some("cmd-alt-b".into())).unwrap();
        assert!(restored.is_empty());
    }

    #[test]
    fn command_1_moves_from_the_tab_to_the_agent() {
        let next = assign(
            &KeyOverrides::default(),
            "selectAgent1",
            Some("cmd-1".into()),
        )
        .unwrap();
        assert_eq!(
            bound(&next, "selectAgent1").as_deref(),
            Some(chord("cmd-1")).as_deref()
        );
        assert_eq!(bound(&next, "selectTab1"), None);
        assert_eq!(
            bound(&next, "selectTab2").as_deref(),
            Some(chord("cmd-2")).as_deref()
        );
        assert_eq!(next.get("selectTab1"), Some(""));
        // Clearing the agent does not hand the chord back.
        let cleared = assign(&next, "selectAgent1", None).unwrap();
        assert_eq!(bound(&cleared, "selectAgent1"), None);
        assert_eq!(bound(&cleared, "selectTab1"), None);
    }

    #[test]
    fn a_reserved_chord_is_refused_and_a_plain_key_is_not_a_shortcut() {
        let overrides = KeyOverrides::default();
        let err = assign(&overrides, "toggleChanges", Some("cmd-n".into())).unwrap_err();
        assert!(err.contains("New agent"));
        assert!(assign(&overrides, "toggleChanges", Some("r".into())).is_err());
        assert!(assign(&overrides, "toggleChanges", Some("ctrl-r".into())).is_err());
        let pressed = stroke(
            "n",
            Modifiers {
                control: false,
                alt: false,
                shift: false,
                platform: true,
                function: false,
            },
        );
        assert!(
            chord_from_keystroke(&pressed)
                .unwrap_err()
                .contains("New agent")
        );
        let plain = stroke(
            "r",
            Modifiers {
                control: false,
                alt: false,
                shift: false,
                platform: false,
                function: false,
            },
        );
        assert_eq!(
            chord_from_keystroke(&plain).unwrap_err(),
            "Use a Command shortcut."
        );
    }

    #[test]
    fn an_override_beats_another_rows_default_when_the_file_is_edited() {
        let mut overrides = KeyOverrides::default();
        overrides.set("selectAgent1", "cmd-alt-b");
        assert_eq!(
            bound(&overrides, "selectAgent1").as_deref(),
            Some(chord("cmd-alt-b")).as_deref()
        );
        assert_eq!(bound(&overrides, "toggleChanges"), None);
    }

    #[test]
    fn slots_are_zero_based_and_stop_at_nine() {
        assert_eq!(agent_slot("selectAgent1"), Some(0));
        assert_eq!(agent_slot("selectAgent9"), Some(8));
        assert_eq!(agent_slot("selectAgent10"), None);
        assert_eq!(tab_slot("selectTab1"), Some(0));
        assert_eq!(tab_slot("selectAgent1"), None);
    }

    #[test]
    fn shift_and_option_read_in_the_app_order() {
        assert_eq!(display("cmd-shift-w"), "\u{2318}\u{21e7}W");
        assert_eq!(display("cmd-alt-b"), "\u{2325}\u{2318}B");
        assert_eq!(display("cmd-]"), "\u{2318}]");
    }
}
