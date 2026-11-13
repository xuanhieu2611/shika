//! Messages the author sent while the Lead was Working. The queue lives in the
//! chat view only: it is memory, never stored in history, and the actor, turn
//! ids and crash recovery know nothing about it. A queued message becomes a
//! normal send when the Lead is Ready, through the same path Enter uses.
use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long delivery waits after a refused send before trying again, so a
/// persistent refusal cannot repeat every frame.
pub(crate) const REFUSAL_PAUSE: Duration = Duration::from_secs(2);

/// What Enter (or Send) does with the composer's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    /// Nothing: the Lead is not live, the text is blank, an IME is composing,
    /// or the conversation cannot be saved.
    Ignore,
    /// Submit now.
    Send,
    /// Move the text into the queue.
    Queue,
}

/// The chat's state that decides where Enter goes and when a queued message
/// may go.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Lane {
    pub live: bool,
    pub busy: bool,
    pub saving: bool,
    pub save_failed: bool,
    pub composing: bool,
    pub queued: bool,
}

impl Lane {
    /// Where Enter sends `text`. While a turn runs, a send is in flight, or
    /// older messages still wait, the text queues behind them, so the author
    /// is never locked out and messages keep their order.
    pub fn route(&self, text: &str) -> Route {
        if !self.live || self.composing || self.save_failed || text.trim().is_empty() {
            Route::Ignore
        } else if self.busy || self.saving || self.queued {
            Route::Queue
        } else {
            Route::Send
        }
    }

    /// The Lead is free: live and not replying, sending or failing to save.
    pub fn free(&self) -> bool {
        self.live && !self.busy && !self.saving && !self.save_failed
    }
}

#[derive(Debug, Default)]
pub(super) struct Queue {
    messages: VecDeque<String>,
    /// Delivery waits until this instant after a refusal.
    paused_until: Option<Instant>,
}

impl Queue {
    pub fn push(&mut self, text: String) {
        self.messages.push_back(text);
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.messages.iter()
    }

    /// The message to submit now: the first one, when the Lead is free and no
    /// refusal is pausing delivery. One at a time, so the next waits for the
    /// next Ready.
    pub fn next(&self, lane: &Lane, now: Instant) -> Option<&str> {
        if !lane.free() || self.paused_until.is_some_and(|until| now < until) {
            return None;
        }
        self.messages.front().map(String::as_str)
    }

    /// The first message was accepted: it leaves the queue.
    pub fn delivered(&mut self) {
        self.messages.pop_front();
        self.paused_until = None;
    }

    /// The first message was refused: it stays first, and delivery pauses.
    pub fn refused(&mut self, now: Instant) {
        self.paused_until = Some(now + REFUSAL_PAUSE);
    }

    /// Empties the queue for the composer: the queued messages in order, then
    /// the draft that was already there, joined by blank lines. None when
    /// nothing was queued.
    pub fn restore(&mut self, draft: &str) -> Option<String> {
        if self.messages.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = self.messages.drain(..).collect();
        if !draft.trim().is_empty() {
            parts.push(draft.to_string());
        }
        self.paused_until = None;
        Some(parts.join("\n\n"))
    }
}

/// The sentence Quit and Forget add while messages are queued, or None.
pub(crate) fn warning(queued: usize) -> Option<String> {
    match queued {
        0 => None,
        1 => Some("1 queued message has not been sent to the Lead and will be lost.".into()),
        n => Some(format!(
            "{n} queued messages have not been sent to the Lead and will be lost."
        )),
    }
}

/// The sentence Stop adds while messages are queued, or None.
pub(crate) fn returns_note(queued: usize) -> Option<String> {
    match queued {
        0 => None,
        1 => Some("Its 1 queued message goes back to the composer.".into()),
        n => Some(format!("Its {n} queued messages go back to the composer.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lane(busy: bool, queued: bool) -> Lane {
        Lane {
            live: true,
            busy,
            queued,
            ..Lane::default()
        }
    }

    #[test]
    fn enter_queues_while_working_and_sends_when_free() {
        assert_eq!(lane(true, false).route("hello"), Route::Queue);
        assert_eq!(lane(false, false).route("hello"), Route::Send);
        let sending = Lane {
            saving: true,
            ..lane(false, false)
        };
        assert_eq!(sending.route("hello"), Route::Queue);
    }

    #[test]
    fn enter_queues_behind_older_messages_even_when_free() {
        assert_eq!(lane(false, true).route("later"), Route::Queue);
    }

    #[test]
    fn enter_ignores_blank_text_a_resting_lead_composition_and_save_failure() {
        assert_eq!(lane(true, false).route("  \n "), Route::Ignore);
        let resting = Lane {
            live: false,
            ..lane(true, false)
        };
        assert_eq!(resting.route("hello"), Route::Ignore);
        let composing = Lane {
            composing: true,
            ..lane(true, false)
        };
        assert_eq!(composing.route("hello"), Route::Ignore);
        let failed = Lane {
            save_failed: true,
            ..lane(true, false)
        };
        assert_eq!(failed.route("hello"), Route::Ignore);
    }

    #[test]
    fn messages_queue_in_order_and_wait_while_the_lead_works() {
        let mut queue = Queue::default();
        queue.push("one".into());
        queue.push("two".into());
        let now = Instant::now();
        assert_eq!(queue.next(&lane(true, true), now), None);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.iter().cloned().collect::<Vec<_>>(), ["one", "two"]);
    }

    #[test]
    fn one_message_goes_per_ready_in_order_and_exactly_once() {
        let mut queue = Queue::default();
        for text in ["one", "two", "three"] {
            queue.push(text.into());
        }
        let now = Instant::now();
        let mut sent = Vec::new();
        for _ in 0..3 {
            // Ready: the first message is due.
            let text = queue.next(&lane(false, true), now).map(str::to_string);
            sent.push(text.unwrap());
            queue.delivered();
            // Accepted, so the Lead is Working again and nothing else goes.
            assert_eq!(queue.next(&lane(true, !queue.is_empty()), now), None);
        }
        assert_eq!(sent, ["one", "two", "three"]);
        assert!(queue.is_empty());
        assert_eq!(queue.next(&lane(false, false), now), None);
    }

    #[test]
    fn a_refused_message_stays_first_and_delivery_pauses() {
        let mut queue = Queue::default();
        queue.push("one".into());
        queue.push("two".into());
        let now = Instant::now();
        assert_eq!(queue.next(&lane(false, true), now), Some("one"));
        queue.refused(now);
        assert_eq!(queue.len(), 2);
        assert_eq!(queue.next(&lane(false, true), now), None);
        let later = now + REFUSAL_PAUSE;
        assert_eq!(queue.next(&lane(false, true), later), Some("one"));
        queue.delivered();
        assert_eq!(queue.next(&lane(false, true), later), Some("two"));
    }

    #[test]
    fn nothing_is_due_while_sending_or_after_a_save_failure() {
        let mut queue = Queue::default();
        queue.push("one".into());
        let now = Instant::now();
        let sending = Lane {
            saving: true,
            ..lane(false, true)
        };
        assert_eq!(queue.next(&sending, now), None);
        let failed = Lane {
            save_failed: true,
            ..lane(false, true)
        };
        assert_eq!(queue.next(&failed, now), None);
        let resting = Lane {
            live: false,
            ..lane(false, true)
        };
        assert_eq!(queue.next(&resting, now), None);
    }

    #[test]
    fn restoring_joins_the_queue_then_the_draft_with_blank_lines() {
        let mut queue = Queue::default();
        queue.push("one".into());
        queue.push("two\nlines".into());
        assert_eq!(
            queue.restore("draft").as_deref(),
            Some("one\n\ntwo\nlines\n\ndraft")
        );
        assert!(queue.is_empty());
        queue.push("only".into());
        assert_eq!(queue.restore("  ").as_deref(), Some("only"));
        assert_eq!(queue.restore("draft"), None);
    }

    #[test]
    fn the_warning_appears_only_while_messages_are_queued() {
        assert_eq!(warning(0), None);
        assert!(warning(1).unwrap().starts_with("1 queued message has"));
        assert!(warning(3).unwrap().starts_with("3 queued messages have"));
        assert_eq!(returns_note(0), None);
        assert!(returns_note(2).unwrap().contains("2 queued messages"));
    }
}
