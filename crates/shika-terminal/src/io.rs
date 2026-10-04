//! A writer thread for PTY input, so typing on the UI thread and query
//! replies on the reader thread never block on a full PTY buffer.

use std::io::{self, Write};
use std::sync::mpsc::{self, Sender};
use std::thread;

pub struct PtyWriter {
    tx: Sender<Vec<u8>>,
}

impl PtyWriter {
    /// Own `writer` on a new thread. The thread ends when the `PtyWriter`
    /// is dropped or a write fails (the program closed its side).
    pub fn spawn(mut writer: Box<dyn Write + Send>) -> io::Result<Self> {
        let (tx, rx) = mpsc::channel::<Vec<u8>>();
        thread::Builder::new()
            .name("shika-pty-writer".into())
            .spawn(move || {
                while let Ok(mut bytes) = rx.recv() {
                    // Batch whatever else is already queued into one write.
                    while let Ok(more) = rx.try_recv() {
                        bytes.extend_from_slice(&more);
                    }
                    if writer
                        .write_all(&bytes)
                        .and_then(|_| writer.flush())
                        .is_err()
                    {
                        break;
                    }
                }
            })?;
        Ok(Self { tx })
    }

    pub fn write(&self, bytes: &[u8]) {
        let _ = self.tx.send(bytes.to_vec());
    }
}
