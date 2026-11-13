//! History scrolling in the real chat view: the wheel owns the position once
//! the author scrolls up, and the bottom is followed only from the bottom.
use super::*;
use crate::appearance::chrome_for;
use gpui::{
    Entity, EntityInputHandler, IntoElement, ParentElement, ScrollDelta, ScrollWheelEvent, Styled,
    TestAppContext, VisualTestContext, point, size,
};
use shika_core::{Appearance, Core};
use shika_terminal::catalog;

fn records(range: std::ops::Range<i64>) -> Vec<Message> {
    prepare(
        range
            .map(|id| ChatRecord {
                id,
                turn: id,
                timestamp: 1_000 + id as u64,
                segment: 0,
                content: if id % 2 == 0 {
                    ChatContent::Author {
                        text: format!("message {id}"),
                    }
                } else {
                    ChatContent::Lead {
                        text: format!(
                            "reply {id}\n\nsecond paragraph of reply {id} with more words in it"
                        ),
                    }
                },
            })
            .collect(),
    )
}

fn make(cx: &mut VisualTestContext) -> Entity<LeadChat> {
    let path = std::env::temp_dir().join(format!(
        "shika-chat-scroll-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let core = Arc::new(Core::open(&path).unwrap());
    let chrome = chrome_for(&Appearance::default(), &catalog::THEMES[0], false);
    cx.new(|cx| {
        let mut chat = LeadChat::new("p".into(), core, chrome, cx);
        chat.live = true;
        chat.epoch = 1;
        chat
    })
}

fn draw(cx: &mut VisualTestContext, chat: &Entity<LeadChat>) {
    let chat = chat.clone();
    cx.draw(
        point(px(0.), px(0.)),
        size(px(700.), px(600.)),
        move |_, _| {
            gpui::div()
                .size_full()
                .flex()
                .flex_col()
                .child(chat)
                .into_any_element()
        },
    );
}

fn wheel(cx: &mut VisualTestContext, dy: f32) {
    wheel_at(cx, 300., 200., dy)
}
fn wheel_at(cx: &mut VisualTestContext, x: f32, y: f32, dy: f32) {
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(x), px(y)),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        ..Default::default()
    });
}

fn top(chat: &Entity<LeadChat>, cx: &mut VisualTestContext) -> (usize, f32) {
    chat.read_with(cx, |c, _| {
        let top = c.list.logical_scroll_top();
        (top.item_ix, f32::from(top.offset_in_item))
    })
}
fn following(chat: &Entity<LeadChat>, cx: &mut VisualTestContext) -> bool {
    chat.read_with(cx, |c, _| c.list.is_following_tail())
}
fn viewport(chat: &Entity<LeadChat>, cx: &mut VisualTestContext) -> f32 {
    chat.read_with(cx, |c, _| f32::from(c.list.viewport_bounds().size.height))
}
fn seeded(cx: &mut VisualTestContext) -> Entity<LeadChat> {
    let chat = make(cx);
    chat.update(cx, |c, cx| c.add_records(records(1..61), false, cx));
    draw(cx, &chat);
    chat
}

#[gpui::test]
fn a_new_chat_follows_its_newest_message(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    assert!(following(&chat, cx));
    // Rows, the Working indicator and a final response all stay at the bottom.
    chat.update(cx, |c, _| c.busy = true);
    chat.update(cx, |c, cx| c.add_records(records(61..63), false, cx));
    draw(cx, &chat);
    assert!(following(&chat, cx));
    assert_eq!(
        chat.read_with(cx, |c, _| c.list.logical_scroll_top().item_ix),
        chat.read_with(cx, |c, _| c.rows.len())
    );
}

#[gpui::test]
fn scrolling_up_holds_the_position_through_redraws_status_and_new_rows(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    wheel(cx, 120.);
    draw(cx, &chat);
    assert!(!following(&chat, cx));
    let held = top(&chat, cx);
    for _ in 0..3 {
        draw(cx, &chat);
    }
    chat.update(cx, |c, cx| {
        c.busy = true;
        cx.notify();
    });
    draw(cx, &chat);
    chat.update(cx, |c, cx| c.add_records(records(61..64), false, cx));
    draw(cx, &chat);
    chat.update(cx, |c, cx| {
        c.busy = false;
        cx.notify();
    });
    draw(cx, &chat);
    assert!(!following(&chat, cx));
    assert_eq!(top(&chat, cx), held);
}

#[gpui::test]
fn the_new_messages_button_does_not_resize_the_list(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    let full = viewport(&chat, cx);
    wheel(cx, 2.);
    draw(cx, &chat);
    assert!(!following(&chat, cx));
    assert_eq!(viewport(&chat, cx), full);
    // And back at the bottom, following resumes at the same size.
    wheel(cx, -2000.);
    draw(cx, &chat);
    draw(cx, &chat);
    assert!(following(&chat, cx));
    assert_eq!(viewport(&chat, cx), full);
}

#[gpui::test]
fn small_wheel_steps_move_smoothly_in_both_directions(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    let mut last = top(&chat, cx);
    let mut positions = Vec::new();
    for _ in 0..12 {
        wheel(cx, 2.);
        draw(cx, &chat);
        let now = top(&chat, cx);
        assert!(now < last, "{now:?} should be above {last:?}");
        positions.push(now);
        last = now;
    }
    for expected in positions.iter().rev().skip(1) {
        wheel(cx, -2.);
        draw(cx, &chat);
        assert_eq!(top(&chat, cx), *expected);
    }
}

#[gpui::test]
fn sending_returns_to_the_bottom_from_an_older_anchor(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    wheel(cx, 400.);
    draw(cx, &chat);
    assert!(!following(&chat, cx));
    let composer = chat.read_with(cx, |c, _| c.composer.clone());
    cx.update(|window, app| {
        composer.update(app, |input, cx| {
            input.replace_text_in_range(None, "hello", window, cx)
        })
    });
    chat.update(cx, |c, cx| c.submit(cx));
    draw(cx, &chat);
    assert!(following(&chat, cx));
}

#[gpui::test]
fn the_composer_scrolls_inside_its_cap_and_leaves_the_list_alone(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let chat = seeded(cx);
    let composer = chat.read_with(cx, |c, _| c.composer.clone());
    let handle = composer.read_with(cx, |c, _| c.scroll_handle());
    let before = viewport(&chat, cx);
    cx.update(|window, app| {
        composer.update(app, |input, cx| {
            input.replace_text_in_range(None, &"line\n".repeat(12), window, cx)
        })
    });
    draw(cx, &chat);
    draw(cx, &chat);
    // Six lines are visible and the rest is a scroll range for the scrollbar,
    // ending at the last line.
    assert_eq!(f32::from(handle.bounds().size.height), 120.);
    assert_eq!(f32::from(handle.max_offset().y), 13. * 20. - 120.);
    assert!(viewport(&chat, cx) < before, "the dock grows by the field");
    assert!(following(&chat, cx));
}
