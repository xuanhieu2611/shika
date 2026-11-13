//! The editor's scrolling: where the caret is kept, and what the wheel owns.
use super::*;
use crate::appearance::chrome_for;
use gpui::{
    Entity, IntoElement, Keystroke, ParentElement, ScrollDelta, ScrollWheelEvent, Styled,
    TestAppContext, VisualTestContext,
};
use shika_core::Appearance;
use shika_terminal::catalog;

const VIEW: f32 = 120.;
/// Twenty 20px lines.
const CONTENT: f32 = 400.;

fn make(cx: &mut VisualTestContext, lines: usize) -> Entity<ChatInput> {
    let chrome = chrome_for(&Appearance::default(), &catalog::THEMES[0], false);
    let text = (0..lines)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    cx.new(|cx| {
        let mut input = ChatInput::new(text, chrome, cx);
        input.max_height(px(VIEW));
        input
    })
}
fn draw(cx: &mut VisualTestContext, input: &Entity<ChatInput>) {
    let input = input.clone();
    cx.draw(
        point(px(0.), px(0.)),
        size(px(400.), px(300.)),
        move |_, _| gpui::div().w(px(400.)).child(input).into_any_element(),
    );
}
fn wheel(cx: &mut VisualTestContext, dy: f32) {
    cx.simulate_event(ScrollWheelEvent {
        position: point(px(50.), px(50.)),
        delta: ScrollDelta::Pixels(point(px(0.), px(dy))),
        ..Default::default()
    });
}
/// Moves the caret as a keystroke would, and asks for it to be shown.
fn caret_to(cx: &mut VisualTestContext, input: &Entity<ChatInput>, index: usize) {
    input.update(cx, |i, _| {
        i.text.cursor = index;
        i.text.anchor = index;
        i.ensure_cursor = true;
    });
}
fn scrolled(input: &Entity<ChatInput>, cx: &mut VisualTestContext) -> f32 {
    input.read_with(cx, |i, _| -f32::from(i.scroll.offset().y))
}
/// The caret's top and bottom, relative to the viewport's top.
fn caret_in_view(input: &Entity<ChatInput>, cx: &mut VisualTestContext) -> (f32, f32) {
    input.read_with(cx, |i, _| {
        let top = i.caret(i.layout.as_ref().unwrap()).unwrap().y - i.scroll.bounds().top();
        (f32::from(top), f32::from(top + px(20.)))
    })
}

#[test]
fn reveal_moves_the_content_only_as_far_as_the_line_needs() {
    let moved = |top, line, from, to| f32::from(reveal(px(top), px(line), px(from), px(to)));
    // Already inside, flush with either edge.
    assert_eq!(moved(40., 20., 0., 120.), 0.);
    assert_eq!(moved(0., 20., 0., 120.), 0.);
    assert_eq!(moved(100., 20., 0., 120.), 0.);
    // Below: up by the overshoot. Above: down by the gap.
    assert_eq!(moved(380., 20., 0., 120.), -280.);
    assert_eq!(moved(110., 20., 0., 120.), -10.);
    assert_eq!(moved(-50., 20., 0., 120.), 50.);
    // Offset viewports use window coordinates.
    assert_eq!(moved(500., 20., 300., 420.), -100.);
    assert_eq!(moved(250., 20., 300., 420.), 50.);
    // A line taller than the viewport lines up with its top.
    assert_eq!(moved(200., 200., 0., 120.), -200.);
}

#[gpui::test]
fn the_editor_scrolls_to_its_last_line_and_no_further(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let input = make(cx, 20);
    draw(cx, &input);
    let max = input.read_with(cx, |i, _| f32::from(i.scroll.max_offset().y));
    assert_eq!(max, CONTENT - VIEW);
    // A wheel flick far past the end stops with the last line in view.
    wheel(cx, -5000.);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), CONTENT - VIEW);
    // Short text has nothing to scroll.
    let short = make(cx, 3);
    draw(cx, &short);
    assert_eq!(
        short.read_with(cx, |i, _| f32::from(i.scroll.max_offset().y)),
        0.
    );
}

#[gpui::test]
fn the_wheel_position_survives_redraws(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let input = make(cx, 20);
    caret_to(cx, &input, 0);
    draw(cx, &input);
    wheel(cx, -60.);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 60.);
    for _ in 0..3 {
        input.update(cx, |_, cx| cx.notify());
        draw(cx, &input);
    }
    assert_eq!(
        scrolled(&input, cx),
        60.,
        "the caret is off screen, and stays so"
    );
    wheel(cx, 20.);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 40.);
}

#[gpui::test]
fn typing_keeps_the_caret_in_view(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let input = make(cx, 20);
    // Typing at the end of tall text.
    let end = input.read_with(cx, |i, _| i.text.value.len());
    caret_to(cx, &input, end);
    draw(cx, &input);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), CONTENT - VIEW);
    let (top, bottom) = caret_in_view(&input, cx);
    assert!(top >= 0. && bottom <= VIEW, "caret {top}..{bottom}");
    // Moving the caret to an earlier line scrolls up to it, not to the top.
    let line_5 = "line 0\nline 1\nline 2\nline 3\nline 4\n".len();
    caret_to(cx, &input, line_5);
    draw(cx, &input);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 100.);
    let (top, bottom) = caret_in_view(&input, cx);
    assert!(top >= 0. && bottom <= VIEW, "caret {top}..{bottom}");
    // Asking again with the caret already in view moves nothing.
    caret_to(cx, &input, line_5);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 100.);
}

#[gpui::test]
fn a_key_that_changes_nothing_does_not_pull_the_editor_back_to_the_caret(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let input = make(cx, 20);
    caret_to(cx, &input, 0);
    draw(cx, &input);
    wheel(cx, -100.);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 100.);
    // Left at the start of the text moves nothing.
    input.update(cx, |i, cx| {
        i.edit_key(&Keystroke::parse("left").unwrap(), cx)
    });
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 100.);
    // A key that does move the caret shows it again.
    input.update(cx, |i, cx| {
        i.edit_key(&Keystroke::parse("right").unwrap(), cx)
    });
    draw(cx, &input);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 0.);
}

#[gpui::test]
fn home_and_end_scroll_without_moving_the_caret(cx: &mut TestAppContext) {
    let cx = cx.add_empty_window();
    let input = make(cx, 20);
    caret_to(cx, &input, 0);
    draw(cx, &input);
    input.update(cx, |i, cx| {
        i.edit_key(&Keystroke::parse("end").unwrap(), cx)
    });
    draw(cx, &input);
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), CONTENT - VIEW);
    assert_eq!(input.read_with(cx, |i, _| i.text.cursor), 0);
    input.update(cx, |i, cx| {
        i.edit_key(&Keystroke::parse("home").unwrap(), cx)
    });
    draw(cx, &input);
    assert_eq!(scrolled(&input, cx), 0.);
}
