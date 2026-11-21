//! The opt-in Ctrl+B workspace shortcuts. Only cards, Changes, and the
//! selected PTY participate; native text fields and overlays keep their keys.
use super::*;
use gpui::Keystroke;

const TIMEOUT: Duration = Duration::from_secs(2);
pub(super) const HINT: &str = "Ctrl+B: n new agent · x close shell tab";

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Style {
    Native,
    Prefix,
}

impl Style {
    pub(super) fn new_agent_key(self) -> &'static str {
        match self {
            Self::Native => "⌘N",
            Self::Prefix => "Ctrl+B, n",
        }
    }

    pub(super) fn close_shell_key(self) -> &'static str {
        match self {
            Self::Native => "⌘W",
            Self::Prefix => "Ctrl+B, x",
        }
    }
}

pub(super) struct Shortcuts {
    pending: Option<Pending>,
    style: Style,
    _watcher: Option<Subscription>,
}

struct Pending {
    deadline: Instant,
    focus: FocusHandle,
    selection_epoch: u64,
}

#[derive(Debug, PartialEq, Eq)]
enum PrefixKey {
    Prefix,
    New,
    CloseShell,
    Cancel,
    Other,
}

fn prefix_key(stroke: &Keystroke) -> PrefixKey {
    let m = stroke.modifiers;
    if m.platform || m.alt || m.shift || m.function {
        return PrefixKey::Other;
    }
    match (m.control, stroke.key.as_str()) {
        (true, "b") => PrefixKey::Prefix,
        (false, "n") => PrefixKey::New,
        (false, "x") => PrefixKey::CloseShell,
        (false, "escape") => PrefixKey::Cancel,
        _ => PrefixKey::Other,
    }
}

impl Shortcuts {
    pub(super) fn new(style: Style, window: &Window, cx: &mut Context<Shika>) -> Self {
        Self {
            pending: None,
            style,
            _watcher: (style == Style::Prefix).then(|| Self::watch(window, cx)),
        }
    }

    pub(super) fn style(&self) -> Style {
        self.style
    }

    pub(super) fn armed(&self) -> bool {
        self.pending.is_some()
    }

    fn watch(window: &Window, cx: &mut Context<Shika>) -> Subscription {
        let entity = cx.weak_entity();
        let handle = window.window_handle();
        // A directly bound action (e.g. Cmd+B) can take the keystroke before
        // root key capture. Cancel for other keys before GPUI matches them.
        cx.intercept_keystrokes(move |event, window, cx| {
            if window.window_handle() == handle
                && prefix_key(&event.keystroke) == PrefixKey::Other
                && entity
                    .read_with(cx, |this, _| this.prefix.armed())
                    .unwrap_or(false)
            {
                let _ = entity.update(cx, |this, cx| this.cancel_prefix(cx));
            }
        })
    }
}

impl Shika {
    pub(super) fn cancel_prefix(&mut self, cx: &mut Context<Self>) {
        if self.prefix.pending.take().is_some() {
            cx.notify();
        }
    }

    /// Check on key-down as well as on redraw/tick, so a late key cannot
    /// execute on a different surface or selection even before a repaint.
    pub(super) fn expire_prefix(&mut self, now: Instant, window: &Window, cx: &mut Context<Self>) {
        if self.prefix.pending.as_ref().is_some_and(|pending| {
            now >= pending.deadline
                || self.busy
                || self.overlay.is_some()
                || !pending.focus.is_focused(window)
                || self.selection.epoch() != pending.selection_epoch
        }) {
            self.cancel_prefix(cx);
        }
    }

    pub(super) fn prefix_input(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.prefix.style() != Style::Prefix || self.overlay.is_some() || self.busy {
            self.cancel_prefix(cx);
            return false;
        }
        let key = prefix_key(&event.keystroke);
        if key == PrefixKey::Other {
            self.cancel_prefix(cx);
            return false;
        }
        if !self.prefix.armed() && key != PrefixKey::Prefix {
            return false;
        }
        let now = Instant::now();
        self.expire_prefix(now, window, cx);
        let armed = self.prefix.armed();
        if !armed && key != PrefixKey::Prefix {
            return false;
        }
        let Some(focus) = window.focused(cx) else {
            return false;
        };
        let terminal = self.selected_card().and_then(|index| {
            let view = &self.cards[index].active_pane().view;
            (view.focus_handle(cx) == focus).then_some(view)
        });
        if terminal.is_some_and(|view| view.read(cx).is_composing())
            || (terminal.is_none()
                && focus != self.focus
                && !(self.changes.open && focus == self.changes.focus))
        {
            self.cancel_prefix(cx);
            return false;
        }

        // Holding the prefix is not a double tap. A held suffix from
        // before the prefix was armed cannot activate an action either.
        if !event.is_held {
            if !armed {
                self.prefix.pending = Some(Pending {
                    deadline: now + TIMEOUT,
                    focus,
                    selection_epoch: self.selection.epoch(),
                });
                cx.notify();
            } else {
                let terminal = terminal.cloned();
                self.cancel_prefix(cx);
                match key {
                    PrefixKey::New => self.picker(window, cx),
                    PrefixKey::CloseShell => self.close_selected_shell(window, cx),
                    PrefixKey::Prefix => {
                        if let Some(terminal) = terminal {
                            terminal
                                .update(cx, |view, cx| view.send_keystroke(&event.keystroke, cx));
                        }
                    }
                    PrefixKey::Cancel | PrefixKey::Other => {}
                }
            }
        }
        cx.stop_propagation();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[gpui::test]
    fn switching_styles_replaces_only_the_affected_bindings(cx: &mut gpui::TestAppContext) {
        let dir = std::env::temp_dir().join(format!(
            "shika-prefix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&dir).unwrap();
        // A project header is enough to exercise the picker. No CLI or
        // worktree starts, and nothing is read from normal app data.
        let core = Arc::new(Core::open(&dir).unwrap());
        let (app, cx) = cx.add_window_view(|window, cx| {
            let mut app = Shika::new(core, Ok(Settings::default()), None, false, window, cx);
            app.selection = model::Tracked::new(Some(Selection::Project("fixture".into())));
            window.focus(&app.focus, cx);
            app
        });
        let armed = |cx: &gpui::VisualTestContext| app.read_with(cx, |app, _| app.prefix.armed());
        let picker_open = |cx: &gpui::VisualTestContext| {
            app.read_with(cx, |app, _| matches!(app.overlay, Some(Overlay::Picker(_))))
        };
        let shell_chord_bound = |cx: &gpui::VisualTestContext| {
            app.read_with(cx, |_, cx| {
                cx.key_bindings()
                    .borrow()
                    .bindings_for_action(&CloseTerminal)
                    .next()
                    .is_some()
            })
        };
        cx.simulate_keystrokes("ctrl-b n");
        assert!(!picker_open(cx));
        cx.simulate_keystrokes("cmd-n");
        assert!(picker_open(cx));
        assert!(shell_chord_bound(cx));
        cx.simulate_keystrokes("escape");

        // Switch styles and clear a custom shortcut. Both operations refresh
        // the native bindings and must preserve the chosen style.
        cx.simulate_keystrokes("cmd-, ] enter j backspace escape");
        assert!(app.read_with(cx, |app, _| app.core.settings().unwrap().prefix_shortcuts));
        cx.simulate_keystrokes("cmd-n");
        assert!(!picker_open(cx));
        assert!(!shell_chord_bound(cx));
        cx.simulate_keystrokes("ctrl-b");
        assert!(armed(cx));
        cx.simulate_keystrokes("cmd-b n");
        assert!(!armed(cx));
        assert!(!picker_open(cx));
        assert!(app.read_with(cx, |app, _| app.column.hidden));

        cx.simulate_keystrokes("ctrl-b");
        app.update(cx, |app, _| {
            app.prefix.pending.as_mut().unwrap().deadline = Instant::now();
        });
        cx.simulate_keystrokes("n");
        assert!(!picker_open(cx));

        cx.simulate_keystrokes("ctrl-b escape n");
        assert!(!armed(cx));
        assert!(!picker_open(cx));
        cx.simulate_keystrokes("ctrl-b n");
        assert!(!armed(cx));
        assert!(picker_open(cx));
        cx.simulate_keystrokes("escape");
        // The native menu still dispatches this action without a key equivalent.
        cx.dispatch_action(NewAgent);
        assert!(picker_open(cx));
        cx.simulate_keystrokes("escape cmd-, ] h escape ctrl-b n");
        assert!(!picker_open(cx));
        assert!(!app.read_with(cx, |app, _| app.core.settings().unwrap().prefix_shortcuts));
        cx.simulate_keystrokes("cmd-n");
        assert!(picker_open(cx));
        assert!(shell_chord_bound(cx));
        cx.simulate_keystrokes("escape");
        app.update(cx, |app, _| app.control = None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_exact_prefix_and_plain_suffixes_are_recognized() {
        for (binding, key) in [
            ("ctrl-b", PrefixKey::Prefix),
            ("n", PrefixKey::New),
            ("x", PrefixKey::CloseShell),
            ("escape", PrefixKey::Cancel),
            ("b", PrefixKey::Other),
            ("cmd-b", PrefixKey::Other),
            ("ctrl-shift-b", PrefixKey::Other),
            ("ctrl-x", PrefixKey::Other),
            ("shift-n", PrefixKey::Other),
            ("j", PrefixKey::Other),
        ] {
            assert_eq!(prefix_key(&Keystroke::parse(binding).unwrap()), key);
        }
    }
}
