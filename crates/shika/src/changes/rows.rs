//! The row index: a diff flattened into uniform rows once per result, and
//! the visible file ranges that folding maps into it.

use super::{MIN_NUMBER_COLUMNS, columns};
use shika_core::{DiffStat, FileDiff, FileStatus, LineKind, SessionDiff};
use std::{collections::HashSet, ops::Range, sync::Arc};

/// One row of the list. Rows point into `DiffView::files`, so the index
/// costs 16 bytes a row whatever the line length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    /// The empty row before every file header but the first.
    Spacer,
    File(u32),
    FileMeta(u32),
    /// Quiet separation between hunks, with no Git metadata.
    HunkGap(u32),
    /// The rounded bottom of a file card.
    End(u32),
    Line {
        file: u32,
        hunk: u32,
        line: u32,
    },
    /// "Binary file" under a binary file's header.
    Binary(u32),
    /// A file over a cap, expandable: "Large diff hidden", "Large file
    /// hidden", or "Diff hidden" (`collapsed_label`).
    Collapsed(u32),
    /// Lines past the hard limit of an expanded file.
    Cut(u32),
}

/// A diff flattened into rows, built off the UI thread once per result.
#[derive(Debug, Default)]
pub struct DiffView {
    pub files: Vec<Arc<FileDiff>>,
    pub stat: DiffStat,
    pub rows: Vec<Row>,
    /// Row of each file header, in order.
    pub headers: Vec<usize>,
    /// Row of each collapsed file's collapsed row, in order.
    pub collapsed: Vec<usize>,
    /// Each file's widest line in columns, so expanding one file does not
    /// measure the others again.
    widths: Vec<usize>,
    /// The widest line in columns.
    pub columns: usize,
    /// The gutter's width in cells: the widest line number, at least 4.
    pub number_columns: usize,
}

impl DiffView {
    pub fn new(diff: SessionDiff) -> Self {
        let files: Vec<Arc<FileDiff>> = diff.files.into_iter().map(Arc::new).collect();
        let widths = files.iter().map(|file| file_columns(file)).collect();
        Self::index(files, widths, diff.stat)
    }

    /// This view with file `at` replaced, as an expand returns it.
    pub fn with_file(&self, at: usize, file: FileDiff) -> Self {
        let mut files = self.files.clone();
        let mut widths = self.widths.clone();
        if at < files.len() {
            widths[at] = file_columns(&file);
            files[at] = Arc::new(file);
        }
        Self::index(files, widths, self.stat)
    }

    fn index(files: Vec<Arc<FileDiff>>, widths: Vec<usize>, stat: DiffStat) -> Self {
        let lines: usize = files
            .iter()
            .flat_map(|file| &file.hunks)
            .map(|hunk| hunk.lines.len() + 1)
            .sum();
        let mut rows = Vec::with_capacity(lines + files.len() * 3);
        let mut headers = Vec::with_capacity(files.len());
        let mut collapsed = Vec::new();
        let mut widest_number = 0u32;
        for (f, file) in files.iter().enumerate() {
            let at = f as u32;
            if f > 0 {
                rows.push(Row::Spacer);
            }
            headers.push(rows.len());
            rows.push(Row::File(at));
            rows.push(Row::FileMeta(at));
            if file.binary {
                rows.push(Row::Binary(at));
            } else if file.collapsed.is_some() {
                collapsed.push(rows.len());
                rows.push(Row::Collapsed(at));
            } else {
                for (h, hunk) in file.hunks.iter().enumerate() {
                    if h > 0 {
                        rows.push(Row::HunkGap(at));
                    }
                    for (l, line) in hunk.lines.iter().enumerate() {
                        widest_number = widest_number.max(line.old.unwrap_or(0));
                        widest_number = widest_number.max(line.new.unwrap_or(0));
                        rows.push(Row::Line {
                            file: at,
                            hunk: h as u32,
                            line: l as u32,
                        });
                    }
                }
                if file.hidden_lines > 0 {
                    rows.push(Row::Cut(at));
                }
            }
            rows.push(Row::End(at));
        }
        let columns = widths.iter().copied().max().unwrap_or(0);
        Self {
            files,
            stat,
            rows,
            headers,
            collapsed,
            widths,
            columns,
            number_columns: digits(widest_number).max(MIN_NUMBER_COLUMNS),
        }
    }

    /// The file a row belongs to.
    pub fn file_of(&self, row: usize) -> Option<usize> {
        match self.rows.get(row)? {
            Row::Spacer => None,
            Row::File(f)
            | Row::FileMeta(f)
            | Row::HunkGap(f)
            | Row::End(f)
            | Row::Binary(f)
            | Row::Collapsed(f)
            | Row::Cut(f) => Some(*f as usize),
            Row::Line { file, .. } => Some(*file as usize),
        }
    }
}

fn digits(n: u32) -> usize {
    n.checked_ilog10().map_or(1, |log| log as usize + 1)
}

/// The widest line of a file, in columns as painted.
fn file_columns(file: &FileDiff) -> usize {
    file.hunks
        .iter()
        .flat_map(|hunk| &hunk.lines)
        .filter(|line| line.kind != LineKind::NoNewline)
        .map(|line| columns(&line.text))
        .max()
        .unwrap_or(0)
}

/// Visible ranges into the unchanged full index. Folding costs O(files),
/// not O(lines), and never clones code or remeasures it on the UI thread.
#[derive(Clone, Debug, Default)]
pub(super) struct VisibleRows {
    pub(super) spans: Vec<(usize, Range<usize>)>,
    pub(super) len: usize,
    pub(super) headers: Vec<usize>,
    pub(super) collapsed: Vec<usize>,
    pub(super) folded: HashSet<usize>,
}

impl VisibleRows {
    pub(super) fn new(view: &DiffView, folded: &HashSet<(String, FileStatus)>) -> Self {
        let mut visible = Self::default();
        for (f, file) in view.files.iter().enumerate() {
            let start = view.headers[f];
            let end = view
                .headers
                .get(f + 1)
                .map_or(view.rows.len(), |next| next - 1);
            if f > 0 {
                visible.push(start - 1..start);
            }
            visible.headers.push(visible.len);
            if folded.contains(&(file.path.clone(), file.status)) {
                visible.folded.insert(f);
                visible.push(start..start + 2);
                visible.push(end - 1..end);
            } else {
                if file.collapsed.is_some() {
                    visible.collapsed.push(visible.len + 2);
                }
                visible.push(start..end);
            }
        }
        visible
    }

    fn push(&mut self, rows: Range<usize>) {
        self.spans.push((self.len, rows.clone()));
        self.len += rows.len();
    }

    pub(super) fn raw(&self, row: usize) -> Option<usize> {
        if row >= self.len {
            return None;
        }
        let at = self.spans.partition_point(|(start, _)| *start <= row) - 1;
        let (start, range) = &self.spans[at];
        Some(range.start + row - start)
    }

    pub(super) fn position(&self, raw: usize) -> Option<usize> {
        let at = self.spans.partition_point(|(_, range)| range.end <= raw);
        let (start, range) = self.spans.get(at)?;
        range.contains(&raw).then(|| start + raw - range.start)
    }
}

/// Keep the top visible row in place across a fold. If it was inside the
/// newly folded body, fall back to that file's header instead.
pub(super) fn folded_top(
    old: &VisibleRows,
    new: &VisibleRows,
    view: &DiffView,
    top: f32,
    row: f32,
    viewport: f32,
) -> f32 {
    let raw = old.raw((top / row).floor().max(0.) as usize);
    let position = raw.and_then(|raw| new.position(raw));
    let header = raw
        .and_then(|raw| view.file_of(raw))
        .and_then(|f| new.headers.get(f).copied());
    let next = position.map_or_else(
        || header.unwrap_or(0) as f32 * row,
        |at| at as f32 * row + top % row,
    );
    next.clamp(0., (new.len as f32 * row - viewport).max(0.))
}
