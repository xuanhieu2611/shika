//! Painting one row of the list: file card slices, headers, diff lines, and
//! collapsed rows.

use super::{
    DiffView, INSET, Row, card_labels, collapsed_label, lines_label, safe_label, visible_text,
};
use crate::appearance::DiffColors;
use crate::{MONO, Shika, UI_FONT, kbd};
use gpui::{
    AnyElement, FontWeight, InteractiveElement, IntoElement, ParentElement, Pixels, Rgba,
    StatefulInteractiveElement, Styled, WeakEntity, div, prelude::FluentBuilder, px,
};
use shika_core::{FileDiff, FileStatus, LineKind};

/// Everything a row needs to paint, cloned into the list's render closure.
#[derive(Clone)]
pub(super) struct RowPaint {
    pub(super) row: Pixels,
    pub(super) cell: Pixels,
    /// First line column shown, and the part of a cell scrolled past it.
    pub(super) start: usize,
    pub(super) offset: Pixels,
    /// Line columns painted per row, enough to fill the text area.
    pub(super) count: usize,
    pub(super) dim: Rgba,
    pub(super) text: Rgba,
    pub(super) faint: Rgba,
    pub(super) line: Rgba,
    pub(super) hover: Rgba,
    pub(super) header: Rgba,
    pub(super) header_dim: Rgba,
    pub(super) header_white: Rgba,
    pub(super) header_added: Rgba,
    pub(super) header_removed: Rgba,
    pub(super) added: DiffColors,
    pub(super) removed: DiffColors,
    pub(super) shika: WeakEntity<Shika>,
}

impl RowPaint {
    fn base(&self) -> gpui::Div {
        div()
            .h(self.row)
            .w_full()
            .flex()
            .items_center()
            .px(px(INSET))
            .whitespace_nowrap()
            .overflow_hidden()
    }

    pub(super) fn render(&self, view: &DiffView, ix: usize, folded: bool) -> AnyElement {
        let Some(row) = view.rows.get(ix) else {
            return self.base().into_any_element();
        };
        let content = match *row {
            Row::Spacer | Row::End(_) => self.base().into_any_element(),
            Row::File(f) => self.file_header(&view.files[f as usize], f as usize, folded, false),
            Row::FileMeta(f) => self.file_header(&view.files[f as usize], f as usize, folded, true),
            Row::HunkGap(_) => self
                .base()
                .border_t_1()
                .border_color(self.line)
                .into_any_element(),
            Row::Line { file, hunk, line } => self.line(
                &view.files[file as usize].hunks[hunk as usize].lines[line as usize],
                view.number_columns,
            ),
            Row::Binary(_) => self
                .base()
                .text_size(px(11.5))
                .text_color(self.dim)
                .child("Binary file")
                .into_any_element(),
            Row::Collapsed(f) => self.collapsed(ix, f as usize, &view.files[f as usize]),
            Row::Cut(f) => self
                .base()
                .text_size(px(11.5))
                .text_color(self.dim)
                .child(format!(
                    "{} not shown",
                    lines_label(view.files[f as usize].hidden_lines)
                ))
                .into_any_element(),
        };
        if matches!(row, Row::Spacer) {
            return content;
        }
        // Each visible row paints a slice of the card. No per-file entity,
        // nested list, shadow, or offscreen code is laid out.
        div()
            .h(self.row)
            .w_full()
            .px(px(INSET))
            .child(
                div()
                    .h_full()
                    .w_full()
                    .overflow_hidden()
                    .border_l_1()
                    .border_r_1()
                    .border_color(self.line)
                    .when(matches!(row, Row::File(_)), |d| {
                        d.border_t_1().rounded_tl(px(10.)).rounded_tr(px(10.))
                    })
                    .when(matches!(row, Row::End(_)), |d| {
                        d.border_b_1().rounded_bl(px(10.)).rounded_br(px(10.))
                    })
                    .when(
                        matches!(row, Row::File(_) | Row::FileMeta(_))
                            || (folded && matches!(row, Row::End(_))),
                        |d| d.bg(self.header),
                    )
                    .when(matches!(row, Row::FileMeta(_)) && !folded, |d| {
                        d.border_b_1()
                    })
                    .child(content),
            )
            .into_any_element()
    }

    /// Two uniform-height header rows: filename/counts, then directory/status.
    fn file_header(&self, file: &FileDiff, f: usize, folded: bool, metadata: bool) -> AnyElement {
        let shika = self.shika.clone();
        let identity = (file.path.clone(), file.status);
        let hover = self.hover;
        let (name, directory) = card_labels(file);
        let status = match file.status {
            FileStatus::Added => "Added",
            FileStatus::Modified => "Modified",
            FileStatus::Deleted => "Deleted",
            FileStatus::Renamed => "Renamed",
            FileStatus::Untracked => "Untracked",
        };
        let header = self
            .base()
            .id((
                if metadata {
                    "changes-file-meta"
                } else {
                    "changes-file"
                },
                f,
            ))
            .gap(px(8.))
            .cursor_pointer()
            .font_family(MONO)
            .line_height(px(16.))
            .when(!metadata, |d| d.rounded_tl(px(10.)).rounded_tr(px(10.)))
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| {
                let _ = shika.update(cx, |this, cx| {
                    this.toggle_changes_file(identity.clone(), cx)
                });
            });
        if metadata {
            return header
                .text_size(px(10.5))
                .text_color(self.header_dim)
                .child(div().flex_none().w(px(10.)))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_ellipsis_start()
                        .overflow_hidden()
                        .child(directory),
                )
                .child(div().flex_none().font_family(UI_FONT).child(status))
                .into_any_element();
        }
        header
            .text_size(px(12.5))
            .text_color(self.header_white)
            .child(
                div()
                    .flex_none()
                    .w(px(10.))
                    .font_family(UI_FONT)
                    .text_color(self.header_dim)
                    .child(if folded { "›" } else { "⌄" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::MEDIUM)
                    .child(name),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .gap(px(8.))
                    .text_size(px(11.5))
                    .when(file.insertions > 0, |d| {
                        d.child(
                            div()
                                .text_color(self.header_added)
                                .child(format!("+{}", file.insertions)),
                        )
                    })
                    .when(file.deletions > 0, |d| {
                        d.child(
                            div()
                                .text_color(self.header_removed)
                                .child(format!("\u{2212}{}", file.deletions)),
                        )
                    }),
            )
            .into_any_element()
    }

    fn line(&self, line: &shika_core::DiffLine, number_columns: usize) -> AnyElement {
        let gutter = div()
            .flex_none()
            .w(self.cell * (number_columns + 1) as f32)
            .overflow_hidden();
        if line.kind == LineKind::NoNewline {
            return self
                .base()
                .child(gutter)
                .child(div().text_color(self.dim).child(safe_label(&line.text)))
                .into_any_element();
        }
        let (colors, number) = match line.kind {
            LineKind::Added => (Some(self.added), line.new),
            LineKind::Removed => (Some(self.removed), line.old),
            _ => (None, line.new),
        };
        let color = colors.map_or(self.text, |colors| colors.text);
        let number = number.map(|n| n.to_string()).unwrap_or_default();
        let numbers = format!("{number:>number_columns$} ");
        self.base()
            .when_some(colors, |d, colors| d.bg(colors.tint))
            .child(gutter.text_color(self.dim).child(numbers))
            .child(
                div().flex_1().min_w_0().h_full().overflow_hidden().child(
                    div()
                        .relative()
                        .left(-self.offset)
                        .h_full()
                        .flex()
                        .items_center()
                        .text_color(color)
                        .child(visible_text(&line.text, self.start, self.count)),
                ),
            )
            .into_any_element()
    }

    fn collapsed(&self, ix: usize, f: usize, file: &FileDiff) -> AnyElement {
        let shika = self.shika.clone();
        let hover = self.hover;
        let text = collapsed_label(file);
        self.base()
            .id(("changes-collapsed", ix))
            .gap(px(12.))
            .cursor_pointer()
            .hover(move |style| style.bg(hover))
            .on_click(move |_, _, cx| {
                let _ = shika.update(cx, |this, cx| this.expand_changes_file(f, cx));
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.5))
                    .text_color(self.dim)
                    .child(text),
            )
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .font_family(UI_FONT)
                    .text_size(px(11.5))
                    .line_height(px(14.))
                    .text_color(self.faint)
                    .child(kbd("o", self.line, self.dim))
                    .child("show"),
            )
            .into_any_element()
    }
}
