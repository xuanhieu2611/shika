//! One collection pass over a target: resume each file from its checkpoint,
//! stream its records into receipts, and commit counters and offsets together.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use md5::{Digest, Md5};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::super::Date;
use super::QUANTUM;
use super::discovery::input_files;
use super::receipt::{Envelope, receipt};
use super::registry::Target;
use super::stream::{CachedDecoder, DECODERS, JsonLine, PacedFile};

#[derive(Default)]
pub(super) struct Outcome {
    pub(super) more: bool,
    pub(super) pending: bool,
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
    let mut outcome = Outcome {
        more: false,
        pending: discovery_pending,
    };
    let mut bytes = 0;
    for mut file in files {
        tx.execute("INSERT INTO source_files VALUES (?1,?2,?3,?4) ON CONFLICT(source,path) DO UPDATE SET cli=excluded.cli,session=excluded.session",params![target.id(),file.path.to_string_lossy(),file.cli,file.session])?;
        if file.cli == "codex"
            && !file.path.exists()
            && file.path.extension().is_some_and(|e| e == "jsonl")
        {
            let compressed = file.path.with_extension("jsonl.zst");
            if compressed.exists() {
                file.path = compressed;
            }
        }
        if stopped.load(Ordering::Relaxed) {
            return Err(rusqlite::Error::InvalidQuery);
        }
        let path = file.path.to_string_lossy();
        let compressed = file.cli == "codex" && file.path.extension().is_some_and(|e| e == "zst");
        if !compressed && file.cli == "codex" {
            let sibling = file.path.with_extension("jsonl.zst");
            DECODERS.with(|cache| cache.borrow_mut().retain(|c| c.path != sibling));
        }
        let previous: Option<Cursor> = tx
            .query_row(
                "SELECT device,inode,offset,modified,anchors FROM files WHERE path=?1",
                [path.as_ref()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
            .optional()?;
        let read = (|| -> std::io::Result<(File, fs::Metadata)> {
            // Nonblocking prevents raced FIFOs from hanging before type checks.
            let handle = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&file.path)?;
            let metadata = handle.metadata()?;
            if !metadata.is_file() {
                return Err(std::io::ErrorKind::InvalidData.into());
            }
            Ok((handle, metadata))
        })();
        let (mut handle, metadata) = match read {
            Ok(opened) => opened,
            Err(_) => {
                DECODERS.with(|cache| cache.borrow_mut().retain(|c| c.path != file.path));
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                outcome.pending = true;
                continue;
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
            continue;
        }
        if !compressed && offset == metadata.len() {
            let fingerprint =
                anchors(&mut handle, offset).map_err(|_| rusqlite::Error::InvalidQuery)?;
            save_cursor(&tx, &path, &metadata, offset, &fingerprint)?;
            tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
            continue;
        }
        if bytes >= QUANTUM {
            outcome.more = true;
            break;
        }
        let cached = if compressed {
            DECODERS.with(|cache| {
                let mut cache = cache.borrow_mut();
                let found = cache.iter().position(|c| c.path == file.path);
                found.and_then(|i| {
                    let c = cache.remove(i);
                    (c.device == metadata.dev()
                        && c.inode == metadata.ino()
                        && c.stamp == modified(&metadata)
                        && c.offset == offset)
                        .then_some(c.reader)
                })
            })
        } else {
            None
        };
        if compressed && cached.is_none() {
            // Other sources may have lost/replaced a rollout before their next
            // wakeup. Stale readers must not keep the bounded pool full.
            DECODERS.with(|cache| {
                cache.borrow_mut().retain(|c| {
                    fs::symlink_metadata(&c.path).is_ok_and(|m| {
                        m.is_file()
                            && m.dev() == c.device
                            && m.ino() == c.inode
                            && modified(&m) == c.stamp
                    })
                })
            });
        }
        if compressed && cached.is_none() && DECODERS.with(|cache| cache.borrow().len() >= 4) {
            // Yield this source until another decoder drains. Existing sources
            // continue through the scheduler, so history never evicts a reader
            // at every quantum and repeatedly decompresses the same prefix.
            outcome.more = true;
            continue;
        }
        tx.execute_batch("SAVEPOINT input")?;
        let read_result = (|| -> rusqlite::Result<u64> {
            let mut reader = match cached {
                Some(reader) => reader,
                None => {
                    handle
                        .seek(SeekFrom::Start(if compressed { 0 } else { offset }))
                        .map_err(|_| rusqlite::Error::InvalidQuery)?;
                    let raw = PacedFile {
                        file: handle
                            .try_clone()
                            .map_err(|_| rusqlite::Error::InvalidQuery)?,
                        bytes: 0,
                        remaining: metadata.len().saturating_sub(if compressed {
                            0
                        } else {
                            offset
                        }),
                        end: metadata.len(),
                        stopped: stopped.clone(),
                    };
                    let input: Box<dyn Read> = if compressed {
                        let mut decoder = zstd::stream::read::Decoder::new(raw)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?;
                        // Refuse pathological dictionary windows instead of an
                        // unbounded allocation from untrusted compressed metadata.
                        decoder
                            .window_log_max(23)
                            .map_err(|_| rusqlite::Error::InvalidQuery)?;
                        Box::new(PacedFile {
                            file: decoder,
                            bytes: 0,
                            remaining: u64::MAX,
                            end: u64::MAX,
                            stopped: stopped.clone(),
                        })
                    } else {
                        Box::new(raw)
                    };
                    let mut reader = BufReader::new(input);
                    if compressed && offset > 0 {
                        let skipped =
                            std::io::copy(&mut reader.by_ref().take(offset), &mut std::io::sink())
                                .map_err(|_| rusqlite::Error::InvalidQuery)?;
                        if skipped != offset {
                            return Err(rusqlite::Error::InvalidQuery);
                        }
                    }
                    reader
                }
            };
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
                    reader: &mut reader,
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
                        tx.execute_batch("SAVEPOINT receipt")?;
                        match receipt(&tx, &file, record, day) {
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
                if bytes + consumed >= QUANTUM {
                    outcome.more = true;
                    break;
                }
            }
            outcome.pending |= pending;
            if pending {
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
            } else {
                tx.execute("DELETE FROM issues WHERE source=?1", [path.as_ref()])?;
            }
            let fingerprint = if compressed {
                if eof { "eof".to_owned() } else { String::new() }
            } else {
                anchors(&mut handle, offset + consumed)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?
            };
            // A write concurrent with this read makes a checkpoint uncertain. Force
            // a deduplicated replay next time instead of trusting mixed bytes.
            let stamp_unchanged = handle
                .metadata()
                .is_ok_and(|m| modified(&m) == modified(&metadata));
            save_cursor(
                &tx,
                &path,
                &metadata,
                if stamp_unchanged {
                    offset + consumed
                } else {
                    0
                },
                if stamp_unchanged { &fingerprint } else { "" },
            )?;
            if !stamp_unchanged {
                outcome.more = true;
            }
            if compressed && !pending && !eof && stamp_unchanged {
                DECODERS.with(|cache| {
                    let mut cache = cache.borrow_mut();
                    cache.push(CachedDecoder {
                        source: target.id(),
                        path: file.path.clone(),
                        device: metadata.dev(),
                        inode: metadata.ino(),
                        stamp: modified(&metadata),
                        offset: offset + consumed,
                        reader,
                    });
                });
            }
            Ok(consumed)
        })();
        match read_result {
            Ok(consumed) => {
                tx.execute_batch("RELEASE input")?;
                bytes += consumed;
            }
            Err(rusqlite::Error::InvalidQuery) if !stopped.load(Ordering::Relaxed) => {
                tx.execute_batch("ROLLBACK TO input; RELEASE input")?;
                tx.execute("INSERT OR IGNORE INTO issues VALUES (?1)", [path.as_ref()])?;
                outcome.pending = true;
            }
            Err(err) => return Err(err),
        }
    }
    tx.commit()?;
    Ok(outcome)
}
