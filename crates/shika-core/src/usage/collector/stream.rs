//! Streaming readers for provider logs: paced file reads, JSONL record
//! framing, and the small cache of live compressed-rollout decoders.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

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
