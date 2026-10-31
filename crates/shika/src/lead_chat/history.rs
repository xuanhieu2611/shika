//! History paging: loading pages of saved records off the UI thread,
//! merging live records, the scroll anchor, and time separator rows.
use super::{LeadChat, Message, Row, markdown};
use gpui::Context;
use shika_core::{ChatContent, ChatRecord};
use std::collections::HashMap;
use std::sync::Arc;

impl LeadChat {
    pub(super) fn add_records(
        &mut self,
        records: Vec<Message>,
        prepend: bool,
        cx: &mut Context<Self>,
    ) {
        let mut additions = Vec::new();
        let mut replaced = false;
        for record in records {
            if let Some(old) = self
                .records
                .iter_mut()
                .find(|m| m.record.id == record.record.id)
            {
                if old.record.content != record.record.content {
                    *old = record;
                    replaced = true;
                }
            } else {
                additions.push(record);
            }
        }
        let records = additions;
        if records.is_empty() && !replaced {
            return;
        }
        let follow = self.list.is_following_tail();
        let anchor = self.list.logical_scroll_top();
        let anchor_key = self.rows.get(anchor.item_ix).map(|row| self.row_key(row));
        if prepend {
            self.records.splice(0..0, records);
        } else {
            self.records.extend(records);
        }
        self.records.sort_by_key(|m| m.record.id);
        // Bounded pages in memory; retained SQLite history is never pruned.
        if self.records.len() > 180 {
            if prepend {
                self.records.truncate(180);
                self.newer = true;
            } else if follow {
                let remove = self.records.len() - 180;
                self.records.drain(..remove);
                self.older = true;
            } else {
                self.records.truncate(180);
                self.newer = true;
            }
        }
        self.rebuild_rows();
        self.list.reset(self.rows.len());
        if follow && !prepend {
            self.list.scroll_to_end();
        } else {
            let item_ix = anchor_key
                .and_then(|key| self.rows.iter().position(|row| self.row_key(row) == key))
                .unwrap_or(anchor.item_ix.min(self.rows.len()));
            self.list.scroll_to(gpui::ListOffset {
                item_ix,
                offset_in_item: anchor.offset_in_item,
            });
            if !prepend {
                self.newer = true;
            }
        }
        self.text_cache
            .retain(|(id, _), _| self.records.iter().any(|r| r.record.id == *id));
        cx.notify();
    }

    fn row_key(&self, row: &Row) -> (i64, usize) {
        match row {
            Row::Time { id, .. } => (*id, usize::MAX),
            Row::Event { id, .. } => (*id, usize::MAX - 1),
            Row::Block { message, block, .. } => (self.records[*message].record.id, *block),
        }
    }

    pub(super) fn newest(&mut self, cx: &mut Context<Self>) {
        self.records.clear();
        self.rows.clear();
        self.text_cache.clear();
        self.width_cache.clear();
        self.list.reset(0);
        self.list.set_follow_mode(gpui::FollowMode::Tail);
        self.newer = false;
        self.load(None, cx);
    }

    fn rebuild_rows(&mut self) {
        self.rows.clear();
        let mut last_time = 0;
        let mut last_role = None;
        let mut latest_event = HashMap::new();
        for message in &self.records {
            if let ChatContent::Event { key: Some(key), .. } = &message.record.content {
                latest_event.insert(key, message.record.id);
            }
        }
        for (index, message) in self.records.iter().enumerate() {
            let record = &message.record;
            if let ChatContent::Event { key: Some(key), .. } = &record.content
                && latest_event.get(key) != Some(&record.id)
            {
                continue;
            }
            if last_time == 0
                || record.timestamp.saturating_sub(last_time) >= 300
                || local_day(record.timestamp) != local_day(last_time)
            {
                self.rows.push(Row::Time {
                    id: record.id,
                    text: local_time(record.timestamp),
                });
                last_role = None;
            }
            last_time = record.timestamp;
            match &record.content {
                ChatContent::Event { text, .. } => {
                    self.rows.push(Row::Event {
                        id: record.id,
                        text: text.clone(),
                    });
                    last_role = None;
                }
                ChatContent::Author { .. } | ChatContent::Lead { .. } => {
                    let author = matches!(record.content, ChatContent::Author { .. });
                    let gap = if last_role == Some(author) { 6. } else { 20. };
                    for block in 0..message.blocks.len() {
                        self.rows.push(Row::Block {
                            message: index,
                            block,
                            first: block == 0,
                            last: block + 1 == message.blocks.len(),
                            gap,
                        });
                    }
                    last_role = Some(author);
                }
                _ => {}
            }
        }
    }

    pub(super) fn load(&mut self, before: Option<i64>, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.loading = true;
        let core = self.core.clone();
        let project = self.project.clone();
        let epoch = self.epoch;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { core.lead_chat_page(&project, before).map(prepare) })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.epoch != epoch {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(records) => {
                        this.older = records.len() == 60;
                        this.add_records(records, before.is_some(), cx);
                    }
                    Err(error) => {
                        this.error = Some(error.to_string());
                        this.save_failed = true;
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

pub(super) fn prepare(records: Vec<ChatRecord>) -> Vec<Message> {
    records
        .into_iter()
        .map(|record| {
            let lead = matches!(record.content, ChatContent::Lead { .. });
            let blocks = Arc::new(markdown::parse(
                record.content.text().unwrap_or_default(),
                lead,
            ));
            Message { record, blocks }
        })
        .collect()
}

// libc-backed local calendar and locale-aware strftime, without adding a
// second date library. No provider timestamps are used for visible messages.
fn local_day(seconds: u64) -> (i32, i32) {
    let value = local_tm(seconds);
    (value.tm_year, value.tm_yday)
}

fn local_tm(seconds: u64) -> libc::tm {
    let time = seconds as libc::time_t;
    let mut value = std::mem::MaybeUninit::zeroed();
    unsafe {
        libc::localtime_r(&time, value.as_mut_ptr());
        value.assume_init()
    }
}

pub(super) fn local_time(seconds: u64) -> String {
    use objc2_foundation::{NSDate, NSDateFormatter, NSDateFormatterStyle};
    let formatter = NSDateFormatter::new();
    formatter.setDateStyle(NSDateFormatterStyle::MediumStyle);
    formatter.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    formatter.setDoesRelativeDateFormatting(true);
    let date = NSDate::dateWithTimeIntervalSince1970(seconds as f64);
    formatter.stringFromDate(&date).to_string()
}
