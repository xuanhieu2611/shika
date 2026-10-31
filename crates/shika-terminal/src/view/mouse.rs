//! Mouse input: selection, link clicks and hover, mouse reports to the
//! program, and wheel scrolling.

use gpui::{
    Context, DispatchPhase, Hitbox, MouseButton as GpuiButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ScrollDelta, ScrollWheelEvent, Window,
};

use super::{TerminalEvent, TerminalView};
use crate::input::{self, Key, KeyMods, MouseAction, MouseButton};
use crate::types::{SelectionKind, ViewportPoint};

impl TerminalView {
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus_handle, cx);
        let Some(layout) = self.layout else { return };
        let (at, side, _) = layout.cell_at(event.position);
        if click_opens_link(event.button, &event.modifiers)
            && let Some(uri) = self.link_under(at)
        {
            cx.emit(TerminalEvent::OpenLink(uri.to_string()));
            return;
        }
        let modes = self.terminal.modes();
        if modes.mouse_reporting() && !event.modifiers.shift {
            let Some(button) = report_button(event.button) else {
                return;
            };
            if let Some(bytes) = input::encode_mouse(
                Some(button),
                MouseAction::Press,
                at,
                mouse_mods(&event.modifiers),
                &modes,
            ) {
                self.terminal.report(&bytes);
            }
            self.reported_button = Some(button);
            self.last_motion_cell = Some(at);
            return;
        }
        if event.button != GpuiButton::Left {
            return;
        }
        let kind = match event.click_count {
            0 | 1 => SelectionKind::Simple,
            2 => SelectionKind::Word,
            _ => SelectionKind::Line,
        };
        self.terminal.start_selection(kind, at, side);
        self.selecting = true;
        cx.notify();
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        self.note_link_hover(event, hovered, cx);
        let Some(layout) = self.layout else { return };
        if self.selecting {
            if event.pressed_button != Some(GpuiButton::Left) {
                self.selecting = false;
                return;
            }
            // Dragging past the top or bottom edge scrolls history.
            let top = layout.origin.y;
            let bottom = top + layout.line_height * layout.rows as f32;
            if event.position.y < top {
                self.terminal.scroll(1);
            } else if event.position.y >= bottom {
                self.terminal.scroll(-1);
            }
            let (at, side, _) = layout.cell_at(event.position);
            self.terminal.update_selection(at, side);
            cx.notify();
            return;
        }
        // A file dragged over the view is not the user's mouse.
        if cx.has_active_drag() {
            return;
        }
        let modes = self.terminal.modes();
        if !modes.mouse_reporting() || (!hovered && self.reported_button.is_none()) {
            return;
        }
        let (at, _, _) = layout.cell_at(event.position);
        if self.last_motion_cell == Some(at) {
            return;
        }
        self.last_motion_cell = Some(at);
        if let Some(bytes) = input::encode_mouse(
            self.reported_button,
            MouseAction::Motion,
            at,
            mouse_mods(&event.modifiers),
            &modes,
        ) {
            self.terminal.report(&bytes);
        }
    }

    fn link_under(&self, at: ViewportPoint) -> Option<&str> {
        self.snapshot.as_ref()?.lines.get(at.row)?.link_at(at.col)
    }

    fn note_link_hover(&mut self, event: &MouseMoveEvent, hovered: bool, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let over_link = if hovered && (event.modifiers.control || event.modifiers.platform) {
            let (at, _, _) = layout.cell_at(event.position);
            self.link_under(at).is_some()
        } else {
            false
        };
        if over_link != self.link_hover {
            self.link_hover = over_link;
            cx.notify();
        }
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        if self.selecting && event.button == GpuiButton::Left {
            self.selecting = false;
            cx.notify();
        }
        let Some(button) = self.reported_button else {
            return;
        };
        if report_button(event.button) != Some(button) {
            return;
        }
        self.reported_button = None;
        let Some(layout) = self.layout else { return };
        let (at, _, _) = layout.cell_at(event.position);
        let modes = self.terminal.modes();
        if let Some(bytes) = input::encode_mouse(
            Some(button),
            MouseAction::Release,
            at,
            mouse_mods(&event.modifiers),
            &modes,
        ) {
            self.terminal.report(&bytes);
        }
    }

    fn scroll_wheel(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let Some(layout) = self.layout else { return };
        let lines = match event.delta {
            ScrollDelta::Lines(delta) => delta.y,
            ScrollDelta::Pixels(delta) => delta.y / layout.line_height,
        } + self.scroll_remainder;
        let whole = lines.trunc();
        self.scroll_remainder = lines - whole;
        let count = whole.abs() as usize;
        if count == 0 {
            return;
        }
        // Positive is toward older output, the same direction as the
        // content moving down under the fingers.
        let up = whole > 0.0;
        let modes = self.terminal.modes();
        if modes.mouse_reporting() && !event.modifiers.shift {
            let (at, _, _) = layout.cell_at(event.position);
            let button = if up {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            let mods = mouse_mods(&event.modifiers);
            if let Some(bytes) =
                input::encode_mouse(Some(button), MouseAction::Press, at, mods, &modes)
            {
                self.terminal
                    .report(&bytes.repeat(count.min(MAX_WHEEL_REPORTS)));
            }
            return;
        }
        if modes.alt_screen && modes.alternate_scroll {
            let key = if up { Key::Up } else { Key::Down };
            if let Some(bytes) = input::encode_key(key, KeyMods::NONE, &modes) {
                self.terminal
                    .report(&bytes.repeat(count.min(MAX_WHEEL_REPORTS)));
            }
            return;
        }
        self.terminal
            .scroll(if up { count as i32 } else { -(count as i32) });
        cx.notify();
    }
}

const MAX_WHEEL_REPORTS: usize = 10;

fn report_button(button: GpuiButton) -> Option<MouseButton> {
    match button {
        GpuiButton::Left => Some(MouseButton::Left),
        GpuiButton::Middle => Some(MouseButton::Middle),
        GpuiButton::Right => Some(MouseButton::Right),
        GpuiButton::Navigate(_) => None,
    }
}

fn mouse_mods(m: &gpui::Modifiers) -> KeyMods {
    KeyMods {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.control,
    }
}

/// Ctrl-click and Cmd-click open a link. On macOS, GPUI rewrites a
/// control-left click into a right click and clears the control modifier,
/// so a right click on a link is that same gesture.
pub(super) fn click_opens_link(button: GpuiButton, modifiers: &gpui::Modifiers) -> bool {
    match button {
        GpuiButton::Left => modifiers.platform || modifiers.control,
        GpuiButton::Right => true,
        GpuiButton::Middle | GpuiButton::Navigate(_) => false,
    }
}

pub(super) fn register_mouse(
    view: &gpui::Entity<TerminalView>,
    hitbox: &Hitbox,
    window: &mut Window,
) {
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                view.update(cx, |view, cx| view.mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                let hovered = hitbox.is_hovered(window);
                view.update(cx, |view, cx| view.mouse_move(event, hovered, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event({
        let view = view.clone();
        let hitbox = hitbox.clone();
        move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                view.update(cx, |view, cx| view.scroll_wheel(event, cx));
                cx.stop_propagation();
            }
        }
    });
}
