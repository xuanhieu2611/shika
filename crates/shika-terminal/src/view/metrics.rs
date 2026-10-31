//! Font metrics and grid geometry: which font family is used, how big a
//! cell is, where the baseline sits, and where the grid was last painted.

use std::sync::{Arc, OnceLock};

use gpui::{
    App, Bounds, Font, FontFeatures, FontStyle, FontWeight, Pixels, Point, SharedString, Window,
    point, px, size,
};

use super::{TerminalConfig, TerminalView};
use crate::types::{CellFlags, CellSide, ViewportPoint};

/// Font and cell size, measured once per font setting.
#[derive(Clone)]
pub(super) struct Metrics {
    /// Regular, bold, italic, bold italic.
    pub(super) fonts: [Font; 4],
    pub(super) font_size: Pixels,
    pub(super) cell_width: Pixels,
    pub(super) line_height: Pixels,
    pub(super) baseline: Pixels,
    pub(super) underline_offset: Pixels,
}

impl Metrics {
    pub(super) fn font_for(&self, flags: CellFlags) -> usize {
        flags.contains(CellFlags::BOLD) as usize | (flags.contains(CellFlags::ITALIC) as usize) << 1
    }
}

/// Where the grid was last painted, for mouse and IME positions.
#[derive(Clone, Copy)]
pub(super) struct GridLayout {
    pub(super) origin: Point<Pixels>,
    pub(super) cell_width: Pixels,
    pub(super) line_height: Pixels,
    pub(super) rows: usize,
    pub(super) cols: usize,
}

impl GridLayout {
    pub(super) fn cell_at(&self, position: Point<Pixels>) -> (ViewportPoint, CellSide, bool) {
        let x = (position.x - self.origin.x) / self.cell_width;
        let y = (position.y - self.origin.y) / self.line_height;
        let inside = y >= 0.0 && y < self.rows as f32;
        let col = (x.floor().max(0.0) as usize).min(self.cols.saturating_sub(1));
        let row = (y.floor().max(0.0) as usize).min(self.rows.saturating_sub(1));
        let side = if x >= self.cols as f32 || (x >= 0.0 && x.fract() >= 0.5) {
            CellSide::Right
        } else {
            CellSide::Left
        };
        (ViewportPoint { row, col }, side, inside)
    }

    pub(super) fn cell_bounds(&self, row: usize, col: usize, width_cells: usize) -> Bounds<Pixels> {
        Bounds::new(
            point(
                self.origin.x + self.cell_width * col as f32,
                self.origin.y + self.line_height * row as f32,
            ),
            size(self.cell_width * width_cells as f32, self.line_height),
        )
    }
}

impl TerminalView {
    pub(super) fn metrics(&mut self, installed: &[String], window: &mut Window) -> Metrics {
        if let Some(metrics) = &self.metrics {
            return metrics.clone();
        }
        let metrics = measure(&self.config, installed, window);
        self.metrics = Some(metrics.clone());
        metrics
    }
}

/// Installed font family names, read once per process.
///
/// The SF Mono that ships inside macOS is hidden from this list (its family
/// is ".SF NS Mono") and is a variable font; GPUI can only load its regular
/// instance from bytes, so bold would be lost. "SF Mono" therefore matches
/// only when the user installed Apple's static SF Mono faces.
pub(super) fn installed_fonts(cx: &App) -> &'static [String] {
    static NAMES: OnceLock<Vec<String>> = OnceLock::new();
    NAMES.get_or_init(|| cx.text_system().all_font_names())
}

fn measure(config: &TerminalConfig, installed: &[String], window: &mut Window) -> Metrics {
    let family = pick_family(config, installed);
    // Ligatures off: glyphs are placed on the grid one cell at a time, and
    // runs skip blank cells, so a ligature could join text across a gap.
    let features = FontFeatures(Arc::new(vec![("calt".into(), 0), ("liga".into(), 0)]));
    let base = Font {
        family,
        features,
        fallbacks: None,
        weight: FontWeight::NORMAL,
        style: FontStyle::Normal,
    };
    let fonts = [
        base.clone(),
        Font {
            weight: FontWeight::BOLD,
            ..base.clone()
        },
        Font {
            style: FontStyle::Italic,
            ..base.clone()
        },
        Font {
            weight: FontWeight::BOLD,
            style: FontStyle::Italic,
            ..base.clone()
        },
    ];
    let text_system = window.text_system();
    let font_id = text_system.resolve_font(&base);
    let font_size = config.font_size;
    let cell_width = text_system
        .advance(font_id, font_size, 'm')
        .map(|advance| advance.width)
        .unwrap_or(font_size * 0.6);
    let line_height = (font_size * config.line_height).round();
    let ascent = text_system.ascent(font_id, font_size);
    let descent = text_system.descent(font_id, font_size);
    let (baseline, underline_offset) = vertical_align(line_height, ascent, descent);
    Metrics {
        fonts,
        font_size,
        cell_width,
        line_height,
        baseline,
        underline_offset,
    }
}

/// Baseline and underline, both measured down from the top of the cell.
///
/// GPUI's font descent follows OpenType: it is negative when the face hangs
/// below the baseline. `baseline_offset` subtracts that signed value, which
/// pushes the baseline down by the whole descent, so the glyphs sit in the
/// bottom of the cell. The line painter treats descent as a positive
/// distance. This does the same.
pub(super) fn vertical_align(
    line_height: Pixels,
    ascent: Pixels,
    descent: Pixels,
) -> (Pixels, Pixels) {
    let descent = px(f32::from(descent).abs());
    let padding_top = (line_height - ascent - descent) / 2.;
    let baseline = padding_top + ascent;
    let underline = gpui::underline_y_offset(line_height, ascent, descent);
    (baseline, underline)
}

fn pick_family(config: &TerminalConfig, installed: &[String]) -> SharedString {
    config
        .font_families
        .iter()
        .find(|family| installed.iter().any(|name| name == family.as_ref()))
        .cloned()
        .unwrap_or_else(|| "Menlo".into())
}
