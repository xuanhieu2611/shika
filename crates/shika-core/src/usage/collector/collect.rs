//! One collection pass over a target: resume each file from its checkpoint,
//! stream its records into receipts, and commit counters and offsets together.

use std::fs::{self, File};
use std::io::{BufRead, Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::super::Date;
use super::QUANTUM;
use super::discovery::{InputFile, input_files};
use super::receipt::{Envelope, receipt};
use super::registry::Target;
use super::stream::{
    CachedDecoder, DECODERS, JsonLine, MetadataReader, evict_stale_decoders, open_log, open_reader,
    take_decoder,
};

#[derive(Default)]
pub(super) struct Outcome {
    pub(super) more: bool,
    pub(super) pending: bool,
    pub(super) quota: crate::QuotaSnapshot,
}

pub(super) fn modified(metadata: &fs::Metadata) -> String {
    format!(
        "{}:{}:{}:{}",
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

pub(super) type Cursor = (u64, u64, u64, String, String);

pub(super) const ANCHOR_BYTES: u64 = 4096;

// Verify bounded samples of the consumed prefix, never hash all historic
// content on append. The boundary sample includes incomplete-record rewrites.
pub(super) fn anchors(file: &mut File, offset: u64) -> std::io::Result<String> {
    let mut digest = Md5::new();
    let mut buffer = [0; ANCHOR_BYTES as usize];
    for start in [0, offset.saturating_sub(ANCHOR_BYTES)] {
        let length = offset.saturating_sub(start).min(ANCHOR_BYTES) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buffer[..length])?;
        digest.update(start.to_le_bytes());
        digest.update(&buffer[..length]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

pub(super) fn resume_offset(
    previous: &Option<Cursor>,
    metadata: &fs::Metadata,
    handle: &mut File,
    compressed: bool,
) -> std::io::Result<u64> {
    let Some((dev, inode, offset, stamp, fingerprint)) = previous else {
        return Ok(0);
    };
    if *dev != metadata.dev() || *inode != metadata.ino() {
        return Ok(0);
    }
    if compressed {
        // Cold compressed rollouts are immutable. Any physical change replays
        // the stream; receipt deduplication makes that replay safe.
        return Ok(if *stamp == modified(metadata) {
            *offset
        } else {
            0
        });
    }
    if *offset > metadata.len() {
        return Ok(0);
    }
    if *stamp == modified(metadata) {
        return Ok(*offset);
    }
    if !fingerprint.is_empty() && anchors(handle, *offset)? == *fingerprint {
        return Ok(*offset);
    }
    Ok(0)
}

pub(super) fn save_cursor(
    tx: &Transaction<'_>,
    path: &str,
    metadata: &fs::Metadata,
    offset: u64,
    fingerprint: &str,
) -> rusqlite::Result<()> {
    tx.execute("INSERT INTO files(path,device,inode,offset,modified,anchors) VALUES (?1,?2,?3,?4,?5,?6) ON CONFLICT(path) DO UPDATE SET device=excluded.device,inode=excluded.inode,offset=excluded.offset,modified=excluded.modified,anchors=excluded.anchors",params![path,metadata.dev(),metadata.ino(),offset,modified(metadata),fingerprint])?;
    Ok(())
}

/// The checkpoint saved for `path`, if any.
fn load_cursor(tx: &Transaction<'_>, path: &str) -> rusqlite::Result<Option<Cursor>> {
    tx.query_row(
        "SELECT device,inode,offset,modified,anchors FROM files WHERE path=?1",
        [path],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()
}

pub(super) fn collect(
    conn: &mut Connection,
    target: &Target,
    day: Date,
    stopped: &Arc<AtomicBool>,
) -> rusqlite::Result<Outcome> {
    let result = collect_inner(conn, target, day, stopped);
    if result.is_err() {
        // A source can exhaust its bounded retry burst after a store failure.
        // Its uncommitted continuation must not pin a decoder slot forever.
        let source = target.id();
        DECODERS.with(|cache| cache.borrow_mut().retain(|c| c.source != source));
    }
    result
}

pub(super) fn collect_inner(
    conn: &mut Connection,
    target: &Target,
    day: Date,
    stopped: &Arc<AtomicBool>,
) -> rusqlite::Result<Outcome> {
    let (files, discovery_pending) = input_files(conn, target)?;
    let tx = conn.transaction()?;
    let discovery = format!("discovery:{}", target.id());
    if discovery_pending {
        tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [&discovery])?;
    } else {
        tx.execute("DELETE FROM issues WHERE source=?1", [&discovery])?;
    }
    let mut pass = Pass {
        tx: &tx,
        target,
        day,
        stopped,
        outcome: Outcome {
            more: false,
            pending: discovery_pending,
            quota: crate::QuotaSnapshot::default(),
        },
        bytes: 0,
    };
    for file in files {
        if pass.collect_file(file)?.is_break() {
            break;
        }
    }
    let outcome = pass.outcome;
    tx.commit()?;
    Ok(outcome)
}

/// One collection pass over a target's files, inside one store transaction.
struct Pass<'a, 'conn> {
    tx: &'a Transaction<'conn>,
    target: &'a Target,
    day: Date,
    stopped: &'a Arc<AtomicBool>,
    outcome: Outcome,
    /// Bytes consumed so far in this pass, against `QUANTUM`.
    bytes: u64,
}

/// A provider file opened for this pass, and the offset it resumes from.
struct OpenInput<'f> {
    file: &'f InputFile,
    path: &'f str,
    handle: File,
    metadata: fs::Metadata,
    offset: u64,
    compressed: bool,
}

/// How far reading one file's records got.
struct ReadProgress {
    consumed: u64,
    /// The last record is incomplete, so the file is read again later.
    pending: bool,
    eof: bool,
}

impl Pass<'_, '_> {
    /// Read one file from its checkpoint. Break ends the pass at the quantum.
    fn collect_file(&mut self, mut file: InputFile) -> rusqlite::Result<ControlFlow<()>> {
        let tx = self.tx;
        tx.execute("INSERT INTO source_files VALUES (?1,?2,?3,?4) ON CONFLICT(source,path) DO UPDATE SET cli=excluded.cli,session=excluded.session",params![self.target.id(),file.path.to_string_lossy(),file.cli,file.session])?;
        prefer_compressed_rollout(&mut file);
        if self.stopped.load(Ordering::Relaxed) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let path = file.path.to_string_lossy();
        let compressed = file.cli == "codex" && file.path.extension().is_some_and(|e| e == "zst");
        if !compressed && file.cli == "codex" {
            let sibling = file.path.with_extension("jsonl.zst");
            DECODERS.with(|cache| cache.borrow_mut().retain(|c| c.path != sibling));
        }
        let previous = load_cursor(tx, &path)?;
        let (mut handle, metadata) = match open_log(&file.path) {
            Ok(opened) => opened,
            Err(_) => {
                DECODERS.with(|cache| cache.borrow_mut().retain(|c| c.path != file.path));
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                self.outcome.pending = true;
                return Ok(ControlFlow::Continue(()));
            }
        };
        let offset = resume_offset(&previous, &metadata, &mut handle, compressed)
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if compressed
            && previous.as_ref().is_some_and(|c| {
                c.0 == metadata.dev()
                    && c.1 == metadata.ino()
                    && c.3 == modified(&metadata)
                    && c.4 == "eof"
            })
        {
            tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
            return Ok(ControlFlow::Continue(()));
        }
        if !compressed && offset == metadata.len() {
            let fingerprint =
                anchors(&mut handle, offset).map_err(|_| rusqlite::Error::InvalidQuery)?;
            save_cursor(tx, &path, &metadata, offset, &fingerprint)?;
            tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
            return Ok(ControlFlow::Continue(()));
        }
        if self.bytes >= QUANTUM {
            self.outcome.more = true;
            return Ok(ControlFlow::Break(()));
        }
        let cached = if compressed {
            take_decoder(&file.path, &metadata, offset)
        } else {
            None
        };
        if compressed && cached.is_none() {
            evict_stale_decoders();
        }
        if compressed && cached.is_none() && DECODERS.with(|cache| cache.borrow().len() >= 4) {
            // Yield this source until another decoder drains. Existing sources
            // continue through the scheduler, so history never evicts a reader
            // at every quantum and repeatedly decompresses the same prefix.
            self.outcome.more = true;
            return Ok(ControlFlow::Continue(()));
        }
        tx.execute_batch("SAVEPOINT input")?;
        let mut input = OpenInput {
            file: &file,
            path: &path,
            handle,
            metadata,
            offset,
            compressed,
        };
        let read_result = self.read_input(&mut input, cached);
        match read_result {
            Ok(consumed) => {
                tx.execute_batch("RELEASE input")?;
                self.bytes += consumed;
            }
            Err(rusqlite::Error::InvalidQuery) if !self.stopped.load(Ordering::Relaxed) => {
                tx.execute_batch("ROLLBACK TO input; RELEASE input")?;
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                self.outcome.pending = true;
            }
            Err(err) => return Err(err),
        }
        Ok(ControlFlow::Continue(()))
    }

    /// Stream an opened file's records into receipts, then checkpoint how far
    /// it got. Returns the bytes consumed.
    fn read_input(
        &mut self,
        input: &mut OpenInput<'_>,
        cached: Option<MetadataReader>,
    ) -> rusqlite::Result<u64> {
        let mut reader = match cached {
            Some(reader) => reader,
            None => open_reader(
                &mut input.handle,
                &input.metadata,
                input.offset,
                input.compressed,
                self.stopped,
            )?,
        };
        let progress = self.read_records(input, &mut reader)?;
        self.checkpoint(input, reader, &progress)?;
        Ok(progress.consumed)
    }

    /// Read whole records until the end, an incomplete last record, or the
    /// pass's quantum. Each receipt has its own savepoint, so a corrupt one is
    /// noted and skipped.
    fn read_records(
        &mut self,
        input: &OpenInput<'_>,
        reader: &mut MetadataReader,
    ) -> rusqlite::Result<ReadProgress> {
        let tx = self.tx;
        let path = input.path;
        let mut consumed = 0;
        let mut pending = false;
        let mut eof = false;
        loop {
            if reader
                .fill_buf()
                .map_err(|_| rusqlite::Error::InvalidQuery)?
                .is_empty()
            {
                eof = true;
                break;
            }
            let mut line = JsonLine {
                reader: &mut *reader,
                bytes: 0,
                complete: false,
                nonwhite: false,
                until_end: 0,
            };
            let record = serde_json::from_reader::<_, Envelope>(&mut line);
            std::io::copy(&mut line, &mut std::io::sink())
                .map_err(|_| rusqlite::Error::InvalidQuery)?;
            match record {
                Ok(record) => {
                    if input.file.cli == "codex"
                        && line.complete
                        && let Some(snapshot) = record.quota()
                    {
                        self.outcome.quota.merge(snapshot);
                    }
                    tx.execute_batch("SAVEPOINT receipt")?;
                    match receipt(tx, input.file, record, self.day) {
                        Ok(()) => tx.execute_batch("RELEASE receipt")?,
                        Err(rusqlite::Error::InvalidQuery) => {
                            tx.execute_batch("ROLLBACK TO receipt; RELEASE receipt")?;
                            tx.execute(
                                "INSERT OR IGNORE INTO issues VALUES (?1)",
                                [format!("corrupt:{path}")],
                            )?;
                        }
                        Err(err) => return Err(err),
                    }
                }
                Err(err) if err.is_io() => return Err(rusqlite::Error::InvalidQuery),
                Err(_) if !line.nonwhite => {}
                Err(_) if !line.complete => {
                    pending = true;
                    break;
                }
                Err(_) => {
                    tx.execute(
                        "INSERT OR IGNORE INTO issues VALUES (?1)",
                        [format!("corrupt:{path}")],
                    )?;
                }
            }
            consumed += line.bytes;
            if self.bytes + consumed >= QUANTUM {
                self.outcome.more = true;
                break;
            }
        }
        Ok(ReadProgress {
            consumed,
            pending,
            eof,
        })
    }

    /// Record the file's issue state and its new checkpoint, and keep a
    /// compressed reader that stopped mid-stream for the next pass.
    fn checkpoint(
        &mut self,
        input: &mut OpenInput<'_>,
        reader: MetadataReader,
        progress: &ReadProgress,
    ) -> rusqlite::Result<()> {
        let ReadProgress {
            consumed,
            pending,
            eof,
        } = *progress;
        let (tx, path, offset, compressed) = (self.tx, input.path, input.offset, input.compressed);
        self.outcome.pending |= pending;
        if pending {
            tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path])?;
        } else {
            tx.execute("DELETE FROM issues WHERE source=?1", [path])?;
        }
        let fingerprint = if compressed {
            if eof { "eof".to_owned() } else { String::new() }
        } else {
            anchors(&mut input.handle, offset + consumed)
                .map_err(|_| rusqlite::Error::InvalidQuery)?
        };
        // A write concurrent with this read makes a checkpoint uncertain. Force
        // a deduplicated replay next time instead of trusting mixed bytes.
        let stamp_unchanged = input
            .handle
            .metadata()
            .is_ok_and(|m| modified(&m) == modified(&input.metadata));
        save_cursor(
            tx,
            path,
            &input.metadata,
            if stamp_unchanged {
                offset + consumed
            } else {
                0
            },
            if stamp_unchanged { &fingerprint } else { "" },
        )?;
        if !stamp_unchanged {
            self.outcome.more = true;
        }
        if compressed && !pending && !eof && stamp_unchanged {
            DECODERS.with(|cache| {
                let mut cache = cache.borrow_mut();
                cache.push(CachedDecoder {
                    source: self.target.id(),
                    path: input.file.path.clone(),
                    device: input.metadata.dev(),
                    inode: input.metadata.ino(),
                    stamp: modified(&input.metadata),
                    offset: offset + consumed,
                    reader,
                });
            });
        }
        Ok(())
    }
}

/// A Codex rollout whose `.jsonl` is gone may have been compressed in place;
/// read the `.jsonl.zst` beside it instead.
fn prefer_compressed_rollout(file: &mut InputFile) {
    if file.cli == "codex"
        && !file.path.exists()
        && file.path.extension().is_some_and(|e| e == "jsonl")
    {
        let compressed = file.path.with_extension("jsonl.zst");
        if compressed.exists() {
            file.path = compressed;
        }
    }
}
