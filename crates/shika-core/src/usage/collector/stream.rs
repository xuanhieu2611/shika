//! Streaming readers for provider logs: paced file reads, JSONL record
//! framing, and the small cache of live compressed-rollout decoders.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::collect::modified;

/// A large single JSON value is streamed without allocating its content.
/// Yield between 64KB reads so even a giant tool result cannot monopolize CPU
/// or disk. The quantum is enforced between records, not inside a JSON value.
pub(super) struct PacedFile<R = File> {
    pub(super) file: R,
    pub(super) bytes: usize,
    pub(super) remaining: u64,
    pub(super) end: u64,
    pub(super) stopped: Arc<AtomicBool>,
}

impl<R: Read> Read for PacedFile<R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.stopped.load(Ordering::Relaxed) {
            return Err(std::io::ErrorKind::ConnectionAborted.into());
        }
        if self.bytes >= 64 * 1024 {
            thread::sleep(Duration::from_millis(5));
            self.bytes = 0;
        }
        let limit = self.remaining.min(buffer.len() as u64) as usize;
        let read = self.file.read(&mut buffer[..limit])?;
        self.remaining -= read as u64;
        self.bytes += read;
        Ok(read)
    }
}

impl<R: Seek> Seek for PacedFile<R> {
    fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
        let position = self.file.seek(from)?;
        self.remaining = self.end.saturating_sub(position);
        Ok(position)
    }
}

pub(super) type MetadataReader = BufReader<Box<dyn Read>>;

pub(super) struct CachedDecoder {
    pub(super) source: String,
    pub(super) path: PathBuf,
    pub(super) device: u64,
    pub(super) inode: u64,
    pub(super) stamp: String,
    pub(super) offset: u64,
    pub(super) reader: MetadataReader,
}

thread_local! {
    // One worker, at most four live decoder windows. In-progress readers
    // keep their slots until EOF instead of thrashing on a startup backlog.
    // Restart replays from disk checkpoints with receipt deduplication.
    pub(super) static DECODERS: std::cell::RefCell<Vec<CachedDecoder>> = const { std::cell::RefCell::new(Vec::new()) };
}

// JSONL framing lets malformed records be drained without a backward seek.
// Content stays streaming, including giant tool results in compressed files.
pub(super) struct JsonLine<'a> {
    pub(super) reader: &'a mut MetadataReader,
    pub(super) bytes: u64,
    pub(super) complete: bool,
    pub(super) nonwhite: bool,
    pub(super) until_end: usize,
}

impl Read for JsonLine<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.complete || buffer.is_empty() {
            return Ok(0);
        }
        let available = self.reader.fill_buf()?;
        if self.until_end == 0 {
            self.until_end = available
                .iter()
                .position(|b| *b == b'\n')
                .map_or(available.len(), |i| i + 1);
        }
        let length = self.until_end.min(buffer.len());
        self.until_end -= length;
        buffer[..length].copy_from_slice(&available[..length]);
        self.complete = length > 0 && buffer[length - 1] == b'\n';
        self.nonwhite |= buffer[..length].iter().any(|b| !b.is_ascii_whitespace());
        self.reader.consume(length);
        self.bytes += length as u64;
        Ok(length)
    }
}

/// Open a provider log for reading, refusing a symlink or anything but a
/// regular file.
pub(super) fn open_log(path: &Path) -> std::io::Result<(File, fs::Metadata)> {
    // Nonblocking prevents raced FIFOs from hanging before type checks.
    let handle = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let metadata = handle.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok((handle, metadata))
}

/// A paced reader at `offset` of a log's records: a seek for a plain log, a
/// decode from the start that skips `offset` bytes for a compressed rollout.
pub(super) fn open_reader(
    handle: &mut File,
    metadata: &fs::Metadata,
    offset: u64,
    compressed: bool,
    stopped: &Arc<AtomicBool>,
) -> rusqlite::Result<MetadataReader> {
    handle
        .seek(SeekFrom::Start(if compressed { 0 } else { offset }))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let raw = PacedFile {
        file: handle
            .try_clone()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        bytes: 0,
        remaining: metadata
            .len()
            .saturating_sub(if compressed { 0 } else { offset }),
        end: metadata.len(),
        stopped: stopped.clone(),
    };
    let input: Box<dyn Read> = if compressed {
        let mut decoder =
            zstd::stream::read::Decoder::new(raw).map_err(|_| rusqlite::Error::InvalidQuery)?;
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
        let skipped = std::io::copy(&mut reader.by_ref().take(offset), &mut std::io::sink())
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if skipped != offset {
            return Err(rusqlite::Error::InvalidQuery);
        }
    }
    Ok(reader)
}

/// Take the cached decoder for `path` out of the pool. It is returned only
/// when it stopped exactly at `offset` of this same file; a stale one is
/// dropped.
pub(super) fn take_decoder(
    path: &Path,
    metadata: &fs::Metadata,
    offset: u64,
) -> Option<MetadataReader> {
    DECODERS.with(|cache| {
        let mut cache = cache.borrow_mut();
        let found = cache.iter().position(|c| c.path == path);
        found.and_then(|i| {
            let c = cache.remove(i);
            (c.device == metadata.dev()
                && c.inode == metadata.ino()
                && c.stamp == modified(metadata)
                && c.offset == offset)
                .then_some(c.reader)
        })
    })
}

/// Drop cached decoders whose rollout was removed or replaced.
pub(super) fn evict_stale_decoders() {
    // Other sources may have lost/replaced a rollout before their next
    // wakeup. Stale readers must not keep the bounded pool full.
    DECODERS.with(|cache| {
        cache.borrow_mut().retain(|c| {
            fs::symlink_metadata(&c.path).is_ok_and(|m| {
                m.is_file() && m.dev() == c.device && m.ino() == c.inode && modified(&m) == c.stamp
            })
        })
    });
}
