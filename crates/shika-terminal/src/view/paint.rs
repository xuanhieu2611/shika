//! Painting a frame: backgrounds, selection, the cursor, underlines and
//! strikethrough, and IME marked text. Row text is in `glyphs`.

use gpui::{
    App, Bounds, Hsla, Pixels, Rgba, TextAlign, TextRun, UnderlineStyle, Window, fill, outline,
    point, px, size,
};

use super::element::Frame;
use super::glyphs::paint_text;
use super::metrics::GridLayout;
use crate::theme::Palette;
use crate::types::{Cell, CellFlags, CursorShape, Rgb, Snapshot};

pub(super) fn hsla(rgb: Rgb) -> Hsla {
    rgba(rgb, 1.0)
}

fn rgba(rgb: Rgb, alpha: f32) -> Hsla {
    Rgba {
        r: rgb.r as f32 / 255.0,
        g: rgb.g as f32 / 255.0,
        b: rgb.b as f32 / 255.0,
        a: alpha,
    }
    .into()
}

/// Round to the device pixel grid so lines and blocks stay crisp.
pub(super) fn snap(value: Pixels, scale: f32) -> Pixels {
    px((f32::from(value) * scale).round() / scale)
}

pub(super) fn paint_frame(
    bounds: Bounds<Pixels>,
    frame: &Frame,
    window: &mut Window,
    cx: &mut App,
) {
    let snapshot = &frame.snapshot;
    let layout = &frame.layout;
    let palette = &frame.palette;
    let background = snapshot.background;
    window.paint_quad(fill(bounds, rgba(background, frame.background_opacity)));

    // Where the block cursor goes, so the glyph under it can be drawn in
    // the cursor text color. Hidden while composing; the marked text sits
    // there instead.
    let cursor = snapshot.cursor.filter(|_| frame.marked_text.is_none());
    let block_cursor = cursor.filter(|c| frame.focused && c.shape == CursorShape::Block);

    for (row, line) in snapshot.lines.iter().enumerate() {
        paint_backgrounds(row, &line.cells, background, layout, window);
    }
    for (row, line) in snapshot.lines.iter().enumerate() {
        paint_selection(row, &line.cells, palette, layout, window);
    }
    if let Some(c) = block_cursor {
        let width = if c.wide { 2 } else { 1 };
        window.paint_quad(fill(
            layout.cell_bounds(c.row, c.col, width),
            hsla(snapshot.cursor_color),
        ));
    }
    let scale = window.scale_factor();
    for (row, line) in snapshot.lines.iter().enumerate() {
        let cursor_col = block_cursor.filter(|c| c.row == row).map(|c| c.col);
        paint_text(row, line, cursor_col, frame, scale, window);
        paint_decorations(row, &line.cells, cursor_col, frame, window);
    }

    if let Some(c) = cursor
        && block_cursor.is_none()
    {
        let width = if c.wide { 2 } else { 1 };
        let cell = layout.cell_bounds(c.row, c.col, width);
        let color = hsla(snapshot.cursor_color);
        let bar = px(2.).max(px(1.0 / scale));
        match (frame.focused, c.shape) {
            (true, CursorShape::Beam) => {
                window.paint_quad(fill(
                    Bounds::new(cell.origin, size(bar, cell.size.height)),
                    color,
                ));
            }
            (true, CursorShape::Underline) => {
                let origin = point(cell.origin.x, cell.origin.y + cell.size.height - bar);
                window.paint_quad(fill(Bounds::new(origin, size(cell.size.width, bar)), color));
            }
            _ => window.paint_quad(outline(cell, color, gpui::BorderStyle::Solid)),
        }
    }

    if let Some(text) = &frame.marked_text {
        paint_marked_text(text, snapshot, frame, window, cx);
    }
}

fn paint_backgrounds(
    row: usize,
    cells: &[Cell],
    default: Rgb,
    layout: &GridLayout,
    window: &mut Window,
) {
    let mut col = 0;
    while col < cells.len() {
        let color = cells[col].bg;
        let start = col;
        while col < cells.len() && cells[col].bg == color {
            col += 1;
        }
        if color != default {
            window.paint_quad(fill(
                layout.cell_bounds(row, start, col - start),
                hsla(color),
            ));
        }
    }
}

fn paint_selection(
    row: usize,
    cells: &[Cell],
    palette: &Palette,
    layout: &GridLayout,
    window: &mut Window,
) {
    let mut col = 0;
    while col < cells.len() {
        if !cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
            continue;
        }
        let start = col;
        while col < cells.len() && cells[col].flags.contains(CellFlags::SELECTED) {
            col += 1;
        }
        window.paint_quad(fill(
            layout.cell_bounds(row, start, col - start),
            rgba(palette.selection, palette.selection_alpha),
        ));
    }
}

fn paint_decorations(
    row: usize,
    cells: &[Cell],
    cursor_col: Option<usize>,
    frame: &Frame,
    window: &mut Window,
) {
    let layout = &frame.layout;
    let metrics = &frame.metrics;
    let row_top = layout.origin.y + layout.line_height * row as f32;
    let thickness = px(1.);
    let mut col = 0;
    while col < cells.len() {
        let flags = cells[col].flags & (CellFlags::ANY_UNDERLINE | CellFlags::STRIKETHROUGH);
        let fg = cells[col].fg;
        let start = col;
        while col < cells.len()
            && cells[col].fg == fg
            && cells[col].flags & (CellFlags::ANY_UNDERLINE | CellFlags::STRIKETHROUGH) == flags
            && cursor_col != Some(col)
        {
            col += 1;
        }
        if col == start {
            // The block cursor cell: decorate it alone, in the cursor text color.
            col += 1;
        }
        if flags.is_empty() {
            continue;
        }
        let color = if cursor_col == Some(start) {
            hsla(frame.palette.cursor_text)
        } else {
            hsla(fg)
        };
        let x = layout.origin.x + layout.cell_width * start as f32;
        let width = layout.cell_width * (col - start) as f32;
        let underline_y = row_top + metrics.underline_offset;
        if flags.intersects(CellFlags::ANY_UNDERLINE) {
            let wavy = flags.contains(CellFlags::CURLY_UNDERLINE);
            let style = UnderlineStyle {
                thickness,
                color: Some(color),
                wavy,
            };
            window.paint_underline(point(x, underline_y), width, &style);
            if flags.contains(CellFlags::DOUBLE_UNDERLINE) {
                window.paint_underline(point(x, underline_y + thickness * 2.0), width, &style);
            }
        }
        if flags.contains(CellFlags::STRIKETHROUGH) {
            let y = row_top + metrics.baseline - metrics.font_size * 0.3;
            window.paint_quad(fill(
                Bounds::new(point(x, y), size(width, thickness)),
                color,
            ));
        }
    }
}

fn paint_marked_text(
    text: &str,
    snapshot: &Snapshot,
    frame: &Frame,
    window: &mut Window,
    cx: &mut App,
) {
    let layout = &frame.layout;
    let metrics = &frame.metrics;
    let (row, col) = snapshot.cursor.map(|c| (c.row, c.col)).unwrap_or((0, 0));
    let color = hsla(snapshot.foreground);
    let run = TextRun {
        len: text.len(),
        font: metrics.fonts[0].clone(),
        color,
        background_color: None,
        underline: Some(UnderlineStyle {
            thickness: px(1.),
            color: Some(color),
            wavy: false,
        }),
        strikethrough: None,
    };
    let shaped =
        window
            .text_system()
            .shape_line(text.to_string().into(), metrics.font_size, &[run], None);
    let origin = layout.cell_bounds(row, col, 1).origin;
    let width = shaped.width.max(layout.cell_width);
    window.paint_quad(fill(
        Bounds::new(origin, size(width, layout.line_height)),
        hsla(snapshot.background),
    ));
    let _ = shaped.paint(
        origin,
        layout.line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}
