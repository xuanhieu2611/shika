//! Refresh: following the selected card, reading its diff off the UI thread,
//! folding a file, and expanding a collapsed one.

use super::{Content, DiffView, READING_AFTER, Target, VisibleRows, folded_top};
use crate::Shika;
use gpui::{Context, px};
use shika_core::FileStatus;
use std::{
    io::Write,
    sync::Arc,
    time::{Duration, Instant},
};

impl Shika {
    /// What the panel should show for the selected card.
    fn changes_target(&self) -> Target {
        let Some(card) = self.selected_card().map(|i| &self.cards[i]) else {
            return Target::NoAgent;
        };
        if card.creating {
            return Target::Preparing;
        }
        if card.lead.is_some() {
            return Target::Lead;
        }
        match &card.session {
            Some(session) => Target::Session(session.id.clone()),
            None if card.launch_error.is_some() => Target::SetupFailed,
            None => Target::Preparing,
        }
    }

    /// Follow the selected card. Runs on every paint while the panel is
    /// open; it only acts when the card changed, so a newly shown card
    /// drops the old diff at once and starts its own read.
    pub fn sync_changes(&mut self, cx: &mut Context<Self>) {
        if !self.changes.open {
            return;
        }
        let target = self.changes_target();
        if self.changes.target.as_ref() == Some(&target) {
            return;
        }
        self.changes.clear();
        let session = matches!(target, Target::Session(_));
        self.changes.target = Some(target);
        if session {
            self.fetch_changes(cx);
            let generation = self.changes.generation;
            // Nothing for READING_AFTER is blank; then the list says so.
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(READING_AFTER).await;
                let _ = this.update(cx, |this, cx| {
                    if this.changes.generation == generation
                        && matches!(this.changes.content, Content::Reading { .. })
                    {
                        cx.notify();
                    }
                });
            })
            .detach();
        }
    }

    /// Read the shown task's diff again, off the UI thread. The rows on
    /// screen stay until the result arrives; a newer read or another card
    /// makes this one's result stale, and it is dropped.
    pub fn fetch_changes(&mut self, cx: &mut Context<Self>) {
        let Some(Target::Session(id)) = self.changes.target.clone() else {
            return;
        };
        self.changes.generation += 1;
        let generation = self.changes.generation;
        let expanded: Vec<(String, FileStatus)> = self.changes.expanded.iter().cloned().collect();
        let diagnostics = self.changes.diagnostics.clone();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let session = id.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let started = Instant::now();
                    let mut diff = core.session_diff(&id).map_err(|e| error_line(&e))?;
                    let read = started.elapsed();
                    // Files expanded before this refresh stay expanded.
                    for file in diff.files.iter_mut() {
                        if file.collapsed.is_some()
                            && expanded
                                .iter()
                                .any(|(path, status)| *path == file.path && *status == file.status)
                            && let Ok(Some(full)) = core.session_file_diff(&id, &file.key)
                        {
                            *file = full;
                        }
                    }
                    let indexed = Instant::now();
                    let view = DiffView::new(diff);
                    if let Some(path) = diagnostics {
                        note_timing(&path, read, indexed.elapsed(), &view);
                    }
                    Ok::<_, String>(view)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.changes.generation != generation || !this.changes.shows(&session) {
                    return;
                }
                match result {
                    Ok(view) => this.changes.set_view(view),
                    Err(line) => this.changes.content = Content::Failed(line),
                }
                this.changes.expanding.clear();
                cx.notify();
            });
        })
        .detach();
    }

    /// Fold a file without Git reads or rebuilding the full line index.
    pub(super) fn toggle_changes_file(
        &mut self,
        identity: (String, FileStatus),
        cx: &mut Context<Self>,
    ) {
        if self.busy || self.overlay.is_some() || !self.changes.open {
            return;
        }
        let Some(view) = self.changes.view().cloned() else {
            return;
        };
        if !view
            .files
            .iter()
            .any(|file| file.path == identity.0 && file.status == identity.1)
        {
            return;
        }
        if !self.changes.folded.remove(&identity) {
            self.changes.folded.insert(identity);
        }
        let visible = VisibleRows::new(&view, &self.changes.folded);
        if let Some(metrics) = self.changes.metrics {
            let (top, viewport) = self.changes.top_and_viewport();
            let next = folded_top(
                &self.changes.visible,
                &visible,
                &view,
                top,
                f32::from(metrics.row),
                viewport,
            );
            let state = self.changes.scroll.0.borrow();
            let offset = state.base_handle.offset();
            state
                .base_handle
                .set_offset(gpui::point(offset.x, px(-next)));
        }
        self.changes.visible = Arc::new(visible);
        cx.notify();
    }

    /// Expand a collapsed file in place, through `Core::session_file_diff`.
    /// A file that no longer differs means the diff is stale: read it again.
    pub fn expand_changes_file(&mut self, file: usize, cx: &mut Context<Self>) {
        let Some(Target::Session(id)) = self.changes.target.clone() else {
            return;
        };
        let Some(view) = self.changes.view().cloned() else {
            return;
        };
        let Some(diff) = view.files.get(file) else {
            return;
        };
        if diff.collapsed.is_none() || !self.changes.expanding.insert(file) {
            return;
        }
        let identity = (diff.path.clone(), diff.status);
        self.changes.expanded.insert(identity.clone());
        let key = diff.key.clone();
        let core = self.core.clone();
        cx.spawn(async move |this, cx| {
            let session = id.clone();
            let base = view.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    core.session_file_diff(&id, &key)
                        .map(|found| found.map(|full| view.with_file(file, full)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if !this.changes.shows(&session) {
                    return;
                }
                this.changes.expanding.remove(&file);
                let current = this.changes.view().cloned();
                match result {
                    Ok(Some(expanded)) => match current {
                        Some(current) if Arc::ptr_eq(&current, &base) => {
                            this.changes.set_view(expanded);
                        }
                        // A refresh landed meanwhile. Expand the same file
                        // in the new rows if it is still collapsed there.
                        Some(current) => {
                            if let Some(at) = current.files.iter().position(|f| {
                                f.collapsed.is_some()
                                    && f.path == identity.0
                                    && f.status == identity.1
                            }) {
                                this.expand_changes_file(at, cx);
                            }
                        }
                        None => {}
                    },
                    Ok(None) => this.fetch_changes(cx),
                    Err(error) => this.message(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }
}

/// An error as the panel shows it under "Could not read changes": git's
/// first line when there is one.
pub(super) fn error_line(error: &shika_core::Error) -> String {
    match error {
        shika_core::Error::ReadChanges(Some(line)) => line.clone(),
        shika_core::Error::ReadChanges(None) => String::new(),
        other => other.to_string(),
    }
}

/// One line per read in the diagnostics file: timings and sizes only, never
/// paths or content.
fn note_timing(path: &std::path::Path, read: Duration, index: Duration, view: &DiffView) {
    let line = format!(
        "read_ms={} index_ms={} files={} rows={} collapsed={}\n",
        read.as_millis(),
        index.as_millis(),
        view.files.len(),
        view.rows.len(),
        view.collapsed.len()
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}
