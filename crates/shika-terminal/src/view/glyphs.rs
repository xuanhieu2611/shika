//! Painting a row's text: shaped glyphs placed one per cell, and
//! box-drawing characters drawn as shapes so they join across cells.

use gpui::{Bounds, Hsla, PathBuilder, Pixels, TextRun, Window, fill, point, px};

use super::element::Frame;
use super::metrics::Metrics;
use super::paint::{hsla, snap};
use crate::boxdraw::{self, BoxGlyph, Corner};
use crate::types::CellFlags;

/// One glyph run per row. Blank cells are left out of the shaped text, and
/// every glyph is placed on its own cell, so fallback fonts (CJK, emoji)
/// with other advances cannot push the rest of the row off the grid.
pub(super) fn paint_text(
    row: usize,
    line: &crate::types::Line,
    cursor_col: Option<usize>,
    frame: &Frame,
    scale: f32,
    window: &mut Window,
) {
    struct Placed {
        byte: usize,
        col: usize,
        color: Hsla,
    }

    let metrics = &frame.metrics;
    let layout = &frame.layout;
    let mut text = String::new();
    let mut placed: Vec<Placed> = Vec::new();
    let mut runs: Vec<TextRun> = Vec::new();
    let row_top = layout.origin.y + layout.line_height * row as f32;

    for (col, cell) in line.cells.iter().enumerate() {
        if cell.is_blank() {
            continue;
        }
        let color = if cursor_col == Some(col) {
            hsla(frame.palette.cursor_text)
        } else {
            hsla(cell.fg)
        };
        if let Some(shape) = boxdraw::lookup(cell.ch) {
            let width = if cell.flags.contains(CellFlags::WIDE) {
                2
            } else {
                1
            };
            paint_box_glyph(
                shape,
                layout.cell_bounds(row, col, width),
                color,
                metrics,
                scale,
                window,
            );
            continue;
        }
        let start = text.len();
        text.push(cell.ch);
        if let Some(marks) = line.combining_at(col) {
            text.push_str(marks);
        }
        placed.push(Placed {
            byte: start,
            col,
            color,
        });
        let font = &metrics.fonts[metrics.font_for(cell.flags)];
        match runs.last_mut() {
            Some(run) if run.font == *font => run.len += text.len() - start,
            _ => runs.push(TextRun {
                len: text.len() - start,
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }),
        }
    }
    if text.is_empty() {
        return;
    }

    let shaped = window
        .text_system()
        .layout_line(&text, metrics.font_size, &runs, None);
    let baseline_y = row_top + metrics.baseline;
    let mut current: Option<(usize, Pixels)> = None;
    for run in &shaped.runs {
        for glyph in &run.glyphs {
            let index = placed
                .partition_point(|p| p.byte <= glyph.index)
                .saturating_sub(1);
            let Some(cell) = placed.get(index) else {
                continue;
            };
            // The first glyph of a cell sits on the cell; later glyphs of
            // the same cell (combining marks) keep their shaped offset.
            let base_x = match current {
                Some((i, x)) if i == index => x,
                _ => {
                    current = Some((index, glyph.position.x));
                    glyph.position.x
                }
            };
            let x =
                layout.origin.x + layout.cell_width * cell.col as f32 + (glyph.position.x - base_x);
            let origin = point(x, baseline_y + glyph.position.y);
            let _ = if glyph.is_emoji {
                window.paint_emoji(origin, run.font_id, glyph.id, metrics.font_size)
            } else {
                window.paint_glyph(origin, run.font_id, glyph.id, metrics.font_size, cell.color)
            };
        }
    }
}

fn paint_box_glyph(
    shape: BoxGlyph,
    cell: Bounds<Pixels>,
    color: Hsla,
    metrics: &Metrics,
    scale: f32,
    window: &mut Window,
) {
    let light = px(1.0_f32.max((f32::from(metrics.font_size) / 12.0).round()));
    let heavy = light * 2.0;
    let x0 = cell.origin.x;
    let y0 = cell.origin.y;
    let w = cell.size.width;
    let h = cell.size.height;
    let cx = snap(x0 + w / 2.0, scale);
    let cy = snap(y0 + h / 2.0, scale);
    let x1 = x0 + w;
    let y1 = y0 + h;
    let thick = |weight: u8| match weight {
        0 => px(0.),
        1 => light,
        _ => heavy,
    };
    let rect = |left: Pixels, top: Pixels, right: Pixels, bottom: Pixels| {
        Bounds::from_corners(point(left, top), point(right, bottom))
    };
    match shape {
        BoxGlyph::Lines(arms) => {
            let horizontal = thick(arms.left.max(arms.right));
            let vertical = thick(arms.up.max(arms.down));
            if arms.left > 0 {
                let t = thick(arms.left);
                window.paint_quad(fill(
                    rect(x0, cy - t / 2.0, cx + vertical / 2.0, cy + t / 2.0),
                    color,
                ));
            }
            if arms.right > 0 {
                let t = thick(arms.right);
                window.paint_quad(fill(
                    rect(cx - vertical / 2.0, cy - t / 2.0, x1, cy + t / 2.0),
                    color,
                ));
            }
            if arms.up > 0 {
                let t = thick(arms.up);
                window.paint_quad(fill(
                    rect(cx - t / 2.0, y0, cx + t / 2.0, cy + horizontal / 2.0),
                    color,
                ));
            }
            if arms.down > 0 {
                let t = thick(arms.down);
                window.paint_quad(fill(
                    rect(cx - t / 2.0, cy - horizontal / 2.0, cx + t / 2.0, y1),
                    color,
                ));
            }
        }
        BoxGlyph::Arc(corner) => {
            let radius = (w / 2.0).min(h / 2.0);
            let (vertical_end, horizontal_end, dx, dy) = match corner {
                Corner::DownRight => (y1, x1, 1.0, 1.0),
                Corner::DownLeft => (y1, x0, -1.0, 1.0),
                Corner::UpLeft => (y0, x0, -1.0, -1.0),
                Corner::UpRight => (y0, x1, 1.0, -1.0),
            };
            let mut path = PathBuilder::stroke(light);
            path.move_to(point(cx, vertical_end));
            path.line_to(point(cx, cy + radius * dy));
            path.curve_to(point(cx + radius * dx, cy), point(cx, cy));
            path.line_to(point(horizontal_end, cy));
            if let Ok(path) = path.build() {
                window.paint_path(path, color);
            }
        }
        BoxGlyph::Blocks(rects, alpha) => {
            let color = color.opacity(alpha);
            for &(left, top, right, bottom) in rects {
                let bounds = rect(
                    snap(x0 + w * left, scale),
                    snap(y0 + h * top, scale),
                    snap(x0 + w * right, scale),
                    snap(y0 + h * bottom, scale),
                );
                window.paint_quad(fill(bounds, color));
            }
        }
    }
}
