//! The queue in the real chat view: Enter while Working queues the text and
//! empties the composer, delivery takes one message per free Lead, and
//! restoring brings the text back.
use super::scroll_tests::{draw, make, records};
use super::*;
use gpui::{Entity, EntityInputHandler, TestAppContext, VisualTestContext};
use std::cell::RefCell;
use std::rc::Rc;

fn type_text(chat: &Entity<LeadChat>, text: &str, cx: &mut VisualTestContext) {
    let composer = chat.read_with(cx, |c, _| c.composer.clone());
    cx.update(|window, app| {
        composer.update(app, |input, cx| {
            input.replace_text_in_range(None, text, window, cx)
        })
    });
}

fn composer_text(chat: &Entity<LeadChat>, cx: &mut VisualTestContext) -> String {
    let composer = chat.read_with(cx, |c, _| c.composer.clone());
    composer.read_with(cx, |input, _| input.value().to_string())
}

/// The texts of every `ChatAction::Send` the chat emits from now on.
fn sends(chat: &Entity<LeadChat>, cx: &mut VisualTestContext) -> Rc<RefCell<Vec<String>>> {
    let sent = Rc::new(RefCell::new(Vec::new()));
    let log = sent.clone();
    cx.update(|_, app| {
        app.subscribe(chat, move |_, action: &ChatAction, _| {
            if let ChatAction::Send(text) = action {
                log.borrow_mut().push(text.clone());
            }
        })
        .detach();
    });
    sent
}

#[gpui::test]
fn enter_while_working_queues_the_text_and_empties_the_composer(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = make(cx);
    let sent = sends(&chat, cx);
    chat.update(cx, |c, _| c.busy = true);
    type_text(&chat, "first", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    type_text(&chat, "second\nline", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    draw(cx, &chat);
    assert_eq!(composer_text(&chat, cx), "");
    assert!(
        sent.borrow().is_empty(),
        "nothing is submitted while Working"
    );
    chat.read_with(cx, |c, _| {
        assert_eq!(c.queued_len(), 2);
        assert_eq!(
            c.queue.iter().cloned().collect::<Vec<_>>(),
            ["first", "second\nline"]
        );
        // Not a send in flight: the composer stays usable.
        assert!(!c.saving);
    });
}

#[gpui::test]
fn enter_while_free_still_sends_at_once(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = make(cx);
    let sent = sends(&chat, cx);
    type_text(&chat, "hello", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    assert_eq!(*sent.borrow(), ["hello"]);
    assert_eq!(chat.read_with(cx, |c, _| c.queued_len()), 0);
    // A second Enter before the Lead answers queues behind it.
    type_text(&chat, "next", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    assert_eq!(sent.borrow().len(), 1);
    assert_eq!(chat.read_with(cx, |c, _| c.queued_len()), 1);
}

#[gpui::test]
fn queued_messages_go_one_per_free_lead_and_stay_when_refused(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = make(cx);
    chat.update(cx, |c, _| c.busy = true);
    for text in ["one", "two"] {
        type_text(&chat, text, cx);
        chat.update(cx, |c, cx| c.submit(cx));
    }
    let now = Instant::now();
    // Working: nothing is due.
    assert_eq!(chat.read_with(cx, |c, cx| c.next_queued(now, cx)), None);
    chat.update(cx, |c, _| c.busy = false);
    assert_eq!(
        chat.read_with(cx, |c, cx| c.next_queued(now, cx))
            .as_deref(),
        Some("one")
    );
    // Refused: still first, paused for a moment.
    chat.update(cx, |c, _| c.queued_refused(now));
    assert_eq!(chat.read_with(cx, |c, cx| c.next_queued(now, cx)), None);
    assert_eq!(chat.read_with(cx, |c, _| c.queued_len()), 2);
    let later = now + queue::REFUSAL_PAUSE;
    assert_eq!(
        chat.read_with(cx, |c, cx| c.next_queued(later, cx))
            .as_deref(),
        Some("one")
    );
    // Accepted: the Lead works again and the next one waits for Ready.
    chat.update(cx, |c, cx| {
        c.queued_delivered(cx);
        c.busy = true;
    });
    assert_eq!(chat.read_with(cx, |c, cx| c.next_queued(later, cx)), None);
    chat.update(cx, |c, _| c.busy = false);
    assert_eq!(
        chat.read_with(cx, |c, cx| c.next_queued(later, cx))
            .as_deref(),
        Some("two")
    );
}

#[gpui::test]
fn stop_and_an_unexpected_exit_put_the_queue_back_in_the_composer(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = make(cx);
    chat.update(cx, |c, _| c.busy = true);
    for text in ["one", "two"] {
        type_text(&chat, text, cx);
        chat.update(cx, |c, cx| c.submit(cx));
    }
    type_text(&chat, "draft", cx);
    chat.update(cx, |c, cx| c.restore_queued(cx));
    assert_eq!(composer_text(&chat, cx), "one\n\ntwo\n\ndraft");
    assert_eq!(chat.read_with(cx, |c, _| c.queued_len()), 0);

    chat.update(cx, |c, cx| c.restore_queued(cx));
    assert_eq!(
        composer_text(&chat, cx),
        "one\n\ntwo\n\ndraft",
        "idempotent"
    );

    type_text(&chat, "\n\nlater", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    chat.update(cx, |c, cx| c.receive(ChatEvent::Exit, cx));
    assert_eq!(chat.read_with(cx, |c, _| c.queued_len()), 0);
    assert!(composer_text(&chat, cx).ends_with("later"));
}

#[gpui::test]
fn queued_messages_take_room_after_the_history_and_leave_following_alone(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = make(cx);
    chat.update(cx, |c, cx| c.add_records(records(1..61), false, cx));
    draw(cx, &chat);
    let full = chat.read_with(cx, |c, _| f32::from(c.list.viewport_bounds().size.height));
    chat.update(cx, |c, _| c.busy = true);
    type_text(&chat, "waiting", cx);
    chat.update(cx, |c, cx| c.submit(cx));
    draw(cx, &chat);
    draw(cx, &chat);
    let (height, following) = chat.read_with(cx, |c, _| {
        (
            f32::from(c.list.viewport_bounds().size.height),
            c.list.is_following_tail(),
        )
    });
    assert!(height < full, "the queued bubble sits under the history");
    assert!(following);
    // Delivered, the bubble leaves the dock and the history has its room back.
    chat.update(cx, |c, cx| c.queued_delivered(cx));
    draw(cx, &chat);
    draw(cx, &chat);
    let after = chat.read_with(cx, |c, _| f32::from(c.list.viewport_bounds().size.height));
    assert!(after > height);
}
