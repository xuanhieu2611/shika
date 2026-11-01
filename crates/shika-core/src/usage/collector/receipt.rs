//! Provider metadata records and how one receipt becomes counters: receipt
//! deduplication, per-session baselines, and the daily totals.

use rusqlite::{OptionalExtension, Transaction, params};
use serde::Deserialize;

use super::super::{Date, recorded_date};
use super::discovery::InputFile;

#[derive(Default, Debug)]
pub(super) struct Envelope {
    kind: String,
    timestamp: Option<String>,
    message: Option<Message>,
    payload: Option<Payload>,
}

impl<'de> Deserialize<'de> for Envelope {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;
        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = Envelope;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a provider metadata record")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(
                self,
                mut map: M,
            ) -> Result<Envelope, M::Error> {
                let mut record = Envelope::default();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "type" => record.kind = map.next_value()?,
                        "timestamp" => record.timestamp = map.next_value()?,
                        "message" if record.kind.is_empty() || record.kind == "assistant" => {
                            record.message = map.next_value()?;
                        }
                        "payload"
                            if record.kind.is_empty()
                                || matches!(
                                    record.kind.as_str(),
                                    "token_usage_record" | "event_msg"
                                ) =>
                        {
                            record.payload = map.next_value()?;
                        }
                        _ => {
                            map.next_value::<serde::de::IgnoredAny>()?;
                        }
                    }
                }
                Ok(record)
            }
        }
        deserializer.deserialize_map(Visitor)
    }
}

impl Envelope {
    pub(super) fn quota(&self) -> Option<crate::QuotaSnapshot> {
        if self.kind != "event_msg" {
            return None;
        }
        let payload = self.payload.as_ref()?;
        if payload.kind != "token_count" {
            return None;
        }
        let observed = super::super::timestamp_seconds(self.timestamp.as_deref()?)?;
        let snapshot = payload.rate_limits.snapshot(observed);
        (!snapshot.is_empty()).then_some(snapshot)
    }
}

#[derive(Default, Deserialize, Debug)]
pub(super) struct Message {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    usage: Option<ClaudeUsage>,
}

#[derive(Default, Deserialize, Debug)]
pub(super) struct ClaudeUsage {
    input_tokens: i64,
    output_tokens: i64,
    #[serde(default)]
    cache_read_input_tokens: i64,
    #[serde(default)]
    cache_creation_input_tokens: i64,
}

#[derive(Default, Deserialize, Debug)]
pub(super) struct Payload {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    info: Option<TokenInfo>,
    #[serde(default)]
    rate_limits: crate::limits::CodexLimits,
    #[serde(default)]
    response_id: Option<String>,
    #[serde(default)]
    thread_id: Option<String>,
    #[serde(default)]
    usage: Option<CodexUsage>,
}

#[derive(Default, Deserialize, Debug)]
pub(super) struct CodexUsage {
    total_tokens: i64,
}

#[derive(Default, Deserialize, Debug)]
pub(super) struct TokenInfo {
    total_token_usage: CodexUsage,
}

/// Unknown fields (including conversation content) are skipped by Serde's
/// streaming reader, not deserialized into strings or serde_json::Value.
pub(super) fn receipt(
    tx: &Transaction<'_>,
    file: &InputFile,
    record: Envelope,
    day: Date,
) -> rusqlite::Result<()> {
    if file.cli == "codex" && record.kind == "event_msg" {
        return note_legacy_context(tx, file, record.payload);
    }
    let Some((id, session, counts)) =
        receipt_counts(file, &record.kind, record.message, record.payload)?
    else {
        return Ok(());
    };
    if file.cli == "codex" {
        tx.execute(
            "DELETE FROM issues WHERE source=?1",
            [format!("legacy:{session}")],
        )?;
    }
    if id.is_empty() || id.len() > 256 || counts.iter().any(|n| *n < 0) {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let Some((owner, increase)) = store_receipt(tx, file, &id, session, counts)? else {
        return Ok(());
    };
    let delta = add_session_increase(tx, file, &owner, increase)?;
    if delta > 0 {
        let day = record
            .timestamp
            .as_deref()
            .and_then(recorded_date)
            .unwrap_or(day);
        add_day_tokens(tx, file, day, delta)?;
    }
    Ok(())
}

/// A Codex `token_count` event from an older rollout: context counters, not a
/// receipt. Records the gap for a session that has no receipts yet.
fn note_legacy_context(
    tx: &Transaction<'_>,
    file: &InputFile,
    payload: Option<Payload>,
) -> rusqlite::Result<()> {
    if let Some(payload) = payload
        && payload.kind == "token_count"
        && payload
            .info
            .is_some_and(|info| info.total_token_usage.total_tokens > 0)
    {
        // Older rollouts expose context counters, not deduplicable API
        // receipts. Report that gap instead of quietly displaying zero or
        // counting inherited/compaction context as fresh consumption.
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM receipts WHERE cli='codex' AND session=?1)",
            [&file.session],
            |r| r.get(0),
        )?;
        if !seen {
            tx.execute(
                "INSERT OR IGNORE INTO issues VALUES (?1)",
                [format!("legacy:{}", file.session)],
            )?;
        }
    }
    Ok(())
}

/// The receipt id, owning session, and four counters a record carries, or
/// None for a record that is not a receipt of this file's CLI.
fn receipt_counts(
    file: &InputFile,
    kind: &str,
    message: Option<Message>,
    payload: Option<Payload>,
) -> rusqlite::Result<Option<(String, String, [i64; 4])>> {
    Ok(Some(match (file.cli, kind) {
        ("claude", "assistant") => {
            let Some(Message {
                id,
                usage: Some(usage),
            }) = message
            else {
                return Ok(None);
            };
            let Some(id) = id else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            (
                id,
                file.session.clone(),
                [
                    usage.input_tokens,
                    usage.output_tokens,
                    usage.cache_read_input_tokens,
                    usage.cache_creation_input_tokens,
                ],
            )
        }
        ("codex", "token_usage_record") => {
            let Some(Payload {
                response_id: Some(id),
                thread_id: Some(session),
                usage: Some(usage),
                ..
            }) = payload
            else {
                return Err(rusqlite::Error::InvalidQuery);
            };
            // Forked rollouts can contain their parent's old receipts.
            if session != file.session {
                return Ok(None);
            }
            (id, session, [usage.total_tokens, 0, 0, 0])
        }
        _ => return Ok(None),
    }))
}

/// Keep the larger of the saved and new counters for a receipt id. Returns
/// the session that owns the receipt and how much it grew, or None when it
/// did not grow.
fn store_receipt(
    tx: &Transaction<'_>,
    file: &InputFile,
    id: &str,
    session: String,
    counts: [i64; 4],
) -> rusqlite::Result<Option<(String, i64)>> {
    let previous = tx
        .query_row(
            "SELECT session,a,b,c,d FROM receipts WHERE cli=?1 AND id=?2",
            params![file.cli, id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    [row.get::<_, i64>(1)?, row.get(2)?, row.get(3)?, row.get(4)?],
                ))
            },
        )
        .optional()?;
    let (owner, old) = previous.unwrap_or((session, [0; 4]));
    if let Some(&value) = old.iter().find(|&&n| n < 0) {
        return Err(rusqlite::Error::IntegralValueOutOfRange(1, value));
    }
    let next = std::array::from_fn::<_, 4, _>(|i| old[i].max(counts[i]));
    let increase = next
        .iter()
        .zip(old)
        .try_fold(0i64, |sum, (new, old)| sum.checked_add(new - old))
        .ok_or(rusqlite::Error::InvalidQuery)?;
    if increase == 0 {
        return Ok(None);
    }
    tx.execute("INSERT INTO receipts VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(cli,id) DO UPDATE SET a=excluded.a,b=excluded.b,c=excluded.c,d=excluded.d", params![file.cli,id,owner,next[0],next[1],next[2],next[3]])?;
    tx.execute(
        "INSERT OR IGNORE INTO sessions(cli,id) VALUES (?1,?2)",
        params![file.cli, owner],
    )?;
    Ok(Some((owner, increase)))
}

/// Add a receipt's growth to its session's observed total. Returns how much
/// of it is past the session's legacy baseline: the part that counts.
fn add_session_increase(
    tx: &Transaction<'_>,
    file: &InputFile,
    owner: &str,
    increase: i64,
) -> rusqlite::Result<i64> {
    let (observed, baseline): (i64, i64) = tx.query_row(
        "SELECT observed,baseline FROM sessions WHERE cli=?1 AND id=?2",
        params![file.cli, owner],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if observed < 0 || baseline < 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(
            0,
            observed.min(baseline),
        ));
    }
    let next = observed
        .checked_add(increase)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, observed))?;
    let delta = next.max(baseline) - observed.max(baseline);
    tx.execute(
        "UPDATE sessions SET observed=?3 WHERE cli=?1 AND id=?2",
        params![file.cli, owner, next],
    )?;
    Ok(delta)
}

/// Add `delta` tokens to this CLI's total for `day`.
fn add_day_tokens(
    tx: &Transaction<'_>,
    file: &InputFile,
    day: Date,
    delta: i64,
) -> rusqlite::Result<()> {
    let day = day.iso();
    let old: i64 = tx
        .query_row(
            "SELECT tokens FROM days WHERE day=?1 AND cli=?2",
            params![day, file.cli],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if old < 0 {
        return Err(rusqlite::Error::IntegralValueOutOfRange(0, old));
    }
    let total = old
        .checked_add(delta)
        .ok_or(rusqlite::Error::IntegralValueOutOfRange(0, old))?;
    tx.execute("INSERT INTO days VALUES (?1,?2,?3) ON CONFLICT(day,cli) DO UPDATE SET tokens=excluded.tokens", params![day,file.cli,total])?;
    Ok(())
}
