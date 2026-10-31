//! `shika send` and `shika key`: the guard, encoding text and keys as
//! terminal writes, and typing them into a terminal as the Lead.
use super::{LEAD_PUBLISHING, Responder, refused};
use crate::{HostState, Shika, lock};
use gpui::{Context, Window};
use shika_core::control::{Reply, TaskStatus};
use shika_terminal::Modes;
use shika_terminal::input::{Key, KeyMods, encode_key, encode_paste};
use std::time::{Duration, Instant};

/// Pause between a pasted line and its Enter. Some TUIs (Codex's paste-burst
/// handling) read an Enter that arrives inside a paste burst as a newline.
const ENTER_DELAY: Duration = Duration::from_millis(150);

/// Pause between the keys of one `shika key`, so a TUI sees them one by one.
pub(super) const KEY_DELAY: Duration = Duration::from_millis(40);

/// What the Lead needs to know to type into a worker.
pub struct InputState {
    pub status: TaskStatus,
    /// The author typed into that terminal and has not submitted it.
    pub draft: bool,
    /// A `shika pr` is publishing it ([`Card::lead_publishing`]).
    pub publishing: bool,
}

/// Why the Lead may not type into `task` now. `escape_only` is a `key` of
/// nothing but escape, which may interrupt a Working task.
pub fn input_refusal(task: &str, state: &InputState, escape_only: bool) -> Option<String> {
    match state.status {
        TaskStatus::Exited => {
            return Some(format!(
                "{task} has exited, so there is nothing to type into."
            ));
        }
        TaskStatus::Starting => return Some(format!("{task} is still starting. Try again.")),
        _ if state.publishing => return Some(LEAD_PUBLISHING.into()),
        TaskStatus::Working if !escape_only => {
            return Some(format!(
                "{task} is working. Wait for it, or interrupt it with: shika key {task} escape"
            ));
        }
        _ => {}
    }
    state.draft.then(|| {
        format!(
            "The author has typed into {task}'s terminal and not sent it. Do not add to it; tell the author."
        )
    })
}

/// What a successful `shika send` of `text` to `task` answers.
pub(super) fn send_message(task: &str, text: &str, enter: bool) -> String {
    if enter {
        format!(
            "sent {} characters and Enter to {task}.",
            text.chars().count()
        )
    } else {
        format!(
            "typed {} characters into {task}, no Enter.",
            text.chars().count()
        )
    }
}

/// One write to a terminal. `count` sends it through the typed-input capture,
/// so a pasted line and its Enter are a submission like a typed one.
pub struct Step {
    pub bytes: Vec<u8>,
    pub count: bool,
    /// Wait this long before the write.
    pub delay: Duration,
}

/// `text` as a paste (bracketed when the program asked for it), then Enter as
/// a separate, delayed write. Control characters are dropped. Without
/// bracketed paste, line breaks become spaces so none submits early.
pub fn paste_steps(text: &str, enter: bool, modes: &Modes) -> Vec<Step> {
    let clean: String = text
        .replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect();
    let bytes = if modes.bracketed_paste {
        encode_paste(&clean, true)
    } else {
        encode_paste(&clean.replace('\n', " "), false)
    };
    let mut steps = vec![Step {
        bytes,
        count: enter,
        delay: Duration::ZERO,
    }];
    if enter {
        steps.push(Step {
            bytes: b"\r".to_vec(),
            count: true,
            delay: ENTER_DELAY,
        });
    }
    steps
}

/// The bytes the terminal sends for a key name from `shika key`, for the
/// program's current mode (application cursor keys).
pub fn key_bytes(name: &str, modes: &Modes) -> Option<Vec<u8>> {
    let key = match name {
        "enter" => Key::Enter,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "tab" => Key::Tab,
        "space" => Key::Char(' '),
        "backspace" => Key::Backspace,
        other => {
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_lowercase() || c.is_ascii_digit() => Key::Char(c),
                _ => return None,
            }
        }
    };
    encode_key(key, KeyMods::NONE, modes)
}

/// One write per key, in order.
pub fn key_steps(keys: &[String], modes: &Modes) -> Result<Vec<Step>, String> {
    keys.iter()
        .enumerate()
        .map(|(index, name)| {
            key_bytes(name, modes)
                .map(|bytes| Step {
                    bytes,
                    count: false,
                    delay: if index == 0 {
                        Duration::ZERO
                    } else {
                        KEY_DELAY
                    },
                })
                .ok_or_else(|| format!("Unknown key {name}."))
        })
        .collect()
}

/// What `send` or `key` types.
pub(super) enum Input {
    Paste { text: String, enter: bool },
    Keys(Vec<String>),
}

/// What runs on the app thread when a typing finishes.
type Done = Box<dyn FnOnce(&mut Shika, Result<(), String>)>;

/// One write of a typing, checked and counted. A later step is refused if the
/// author typed since the first.
fn write_step(
    core: &shika_core::Core,
    state: &std::sync::Mutex<HostState>,
    step: &Step,
    stamp: Option<Instant>,
) -> Result<(), String> {
    lock(state).inject(core, step, stamp)
}

impl Shika {
    /// `shika send` and `shika key`: only a worker this Lead started, and
    /// only when [`input_refusal`] allows it.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn control_input(
        &mut self,
        project: &str,
        lead_id: &str,
        task: String,
        input: Input,
        message: String,
        reply: Responder,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(card) = self.project_card(project, &task) else {
            let _ = reply.send(refused(format!("No task {task} in this project.")));
            return;
        };
        if card.started_by.as_deref() != Some(lead_id) {
            let _ = reply.send(refused(format!(
                "{task} was not started by this Lead, so it cannot type into it."
            )));
            return;
        }
        let escape_only =
            matches!(&input, Input::Keys(keys) if keys.iter().all(|key| key == "escape"));
        let state = InputState {
            status: card.control_status(),
            draft: lock(&card.agent.state).has_draft(),
            publishing: card.lead_publishing,
        };
        if let Some(reason) = input_refusal(&task, &state, escape_only) {
            let _ = reply.send(refused(reason));
            return;
        }
        let modes = card.agent.terminal.modes();
        let steps = match input {
            Input::Paste { text, enter } => Ok(paste_steps(&text, enter, &modes)),
            Input::Keys(keys) => key_steps(&keys, &modes),
        };
        match steps {
            Ok(steps) => self.type_into(
                task.clone(),
                steps,
                window,
                cx,
                Box::new(move |_, result| {
                    let _ = reply.send(match result {
                        Ok(()) => Reply::Done { message },
                        Err(why) => refused(format!("{task}: {why}")),
                    });
                }),
            ),
            Err(why) => {
                let _ = reply.send(refused(why));
            }
        }
    }

    /// Writes `steps` into the agent terminal of session `id`, as the Lead and
    /// not the author: the writes do not count as the author typing, but those
    /// marked `count` go through the typed-input capture, so a line and its
    /// Enter start a turn exactly as a typed one does. The first step is
    /// written now and the rest after their delays; a later step is dropped
    /// if the author typed in between. `done` runs on the app thread with the
    /// outcome.
    pub(crate) fn type_into(
        &mut self,
        id: String,
        mut steps: Vec<Step>,
        window: &mut Window,
        cx: &mut Context<Self>,
        done: Done,
    ) {
        let Some(state) = self
            .cards
            .iter()
            .find(|card| card.session.as_ref().is_some_and(|s| s.id == id))
            .map(|card| card.agent.state.clone())
        else {
            done(self, Err("that task is gone.".into()));
            return;
        };
        let stamp = lock(&state).last_typed;
        let core = self.core.clone();
        if steps.is_empty() {
            done(self, Ok(()));
            return;
        }
        let first = steps.remove(0);
        let result = write_step(&core, &state, &first, stamp);
        if steps.is_empty() || result.is_err() {
            done(self, result);
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut result = Ok(());
            for step in steps {
                cx.background_executor().timer(step.delay).await;
                result = write_step(&core, &state, &step, stamp);
                if result.is_err() {
                    break;
                }
            }
            let _ = this.update_in(cx, |this, _, _| done(this, result));
        })
        .detach();
    }
}

impl HostState {
    /// Writes a step the Lead sent. It leaves `last_typed` alone, so the
    /// author-typing guards see only the author, and a step marked `count`
    /// goes through the capture that typed input does. Refused when the CLI
    /// is gone or setup is running, or when the author typed since `stamp`.
    pub(super) fn inject(
        &mut self,
        core: &shika_core::Core,
        step: &Step,
        stamp: Option<Instant>,
    ) -> Result<(), String> {
        if self.last_typed != stamp {
            return Err("the author typed into it meanwhile, so nothing more was sent.".into());
        }
        let Some(pty) = self.pty.filter(|_| !self.exited && !self.preparing) else {
            return Err("its CLI has exited.".into());
        };
        if step.count {
            self.capture_typed(&step.bytes, Instant::now());
        }
        core.write(pty, &step.bytes)
            .map_err(|_| "its terminal is closed.".to_string())
    }
}
