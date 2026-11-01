//! The GPUI element: fits the grid to its bounds and takes a snapshot in
//! prepaint, then registers input and paints the frame.

use std::sync::Arc;

use gpui::{
    App, Bounds, CursorStyle, Element, ElementId, GlobalElementId, Hitbox, HitboxBehavior,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, point, px, relative, size,
};

use super::TerminalView;
use super::ime::TerminalInputHandler;
use super::metrics::{GridLayout, Metrics, installed_fonts};
use super::mouse::register_mouse;
use super::paint::paint_frame;
use crate::theme::Palette;
use crate::types::{Snapshot, TerminalSize};

impl TerminalView {
    /// Fit the grid to the bounds, then take a fresh snapshot only if the
    /// screen changed since the last frame.
    fn prepare_frame(
        &mut self,
        bounds: Bounds<Pixels>,
        installed: &[String],
        window: &mut Window,
    ) -> Frame {
        let metrics = self.metrics(installed, window);
        let padding = self.config.padding;
        let inner = Bounds::new(
            point(
                bounds.origin.x + padding.left,
                bounds.origin.y + padding.top,
            ),
            size(
                (bounds.size.width - padding.left - padding.right).max(px(0.)),
                (bounds.size.height - padding.top - padding.bottom).max(px(0.)),
            ),
        );
        let cols = (inner.size.width / metrics.cell_width).floor() as u16;
        let rows = (inner.size.height / metrics.line_height).floor() as u16;
        // A view squeezed to nothing (being laid out, or collapsed) keeps
        // its last size instead of telling the program it has two columns.
        if cols >= TerminalSize::MIN_COLS && rows >= TerminalSize::MIN_ROWS {
            let size = TerminalSize::new(rows, cols).with_cell(
                f32::from(metrics.cell_width).round() as u16,
                f32::from(metrics.line_height).round() as u16,
            );
            self.terminal.resize(size);
        }
        self.stats.frames += 1;
        if self.terminal.take_dirty() || self.snapshot.is_none() {
            self.snapshot = Some(Arc::new(self.terminal.snapshot()));
            self.stats.snapshots += 1;
        }
        let snapshot = self.snapshot.clone().expect("snapshot was just set");
        let layout = GridLayout {
            origin: inner.origin,
            cell_width: metrics.cell_width,
            line_height: metrics.line_height,
            rows: snapshot.rows,
            cols: snapshot.cols,
        };
        self.layout = Some(layout);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        Frame {
            snapshot,
            metrics,
            layout,
            hitbox,
            palette: self.palette,
            background_opacity: self.background_opacity,
            focused: self.focus_handle.is_focused(window) && window.is_window_active(),
            marked_text: self.marked_text.clone(),
        }
    }
}

/// Everything paint needs, gathered in prepaint.
pub struct Frame {
    pub(super) snapshot: Arc<Snapshot>,
    pub(super) metrics: Metrics,
    pub(super) layout: GridLayout,
    pub(super) hitbox: Hitbox,
    pub(super) palette: Palette,
    pub(super) background_opacity: f32,
    pub(super) focused: bool,
    pub(super) marked_text: Option<String>,
}

pub(super) struct TerminalElement {
    pub(super) view: gpui::Entity<TerminalView>,
}

impl IntoElement for TerminalElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let installed = installed_fonts(cx);
        self.view.update(cx, |view, _cx| {
            view.prepare_frame(bounds, installed, window)
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        frame: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus_handle = self.view.read(cx).focus_handle.clone();
        window.handle_input(
            &focus_handle,
            TerminalInputHandler {
                view: self.view.clone(),
            },
            cx,
        );
        let link_hover = self.view.read(cx).link_hover;
        let mouse_cursor = if link_hover {
            CursorStyle::PointingHand
        } else if frame.snapshot.modes.mouse_reporting() {
            CursorStyle::Arrow
        } else {
            CursorStyle::IBeam
        };
        window.set_cursor_style(mouse_cursor, &frame.hitbox);
        register_mouse(&self.view, &frame.hitbox, window);

        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            paint_frame(bounds, frame, window, cx);
        });
    }
}
