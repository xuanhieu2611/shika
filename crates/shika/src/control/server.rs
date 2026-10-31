//! The socket side: the accept loop, one thread per connection, and the
//! queue that hands each request to the UI thread.
use shika_core::control::{Command, ControlDir, ControlError, Reply, read_request, write_reply};
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

/// Connections served at once. Each is a short-lived thread.
const MAX_CONNECTIONS: usize = 32;

/// One request, handed to the UI thread.
pub struct Incoming {
    pub token: String,
    pub command: Command,
    pub reply: Sender<Reply>,
    /// The connection thread's report on the reply: true once it was written
    /// to the client, false if the write failed. Dropped unsent when the
    /// client left first. Only a `wait` reads it.
    pub delivered: Receiver<bool>,
}

/// The socket, its directory, and the queue of requests for the UI thread.
/// Dropping it stops the accept loop and removes the directory.
pub struct Server {
    dir: ControlDir,
    requests: Receiver<Incoming>,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Binds the socket and installs the stable `shika` command under
    /// `data_dir`.
    pub fn start(data_dir: &Path) -> Result<Self, ControlError> {
        let dir = ControlDir::create(data_dir)?;
        let listener = UnixListener::bind(dir.socket_path())
            .map_err(|err| ControlError::Setup(err.to_string()))?;
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let accept_stop = stop.clone();
        std::thread::Builder::new()
            .name("shika-control".into())
            .spawn(move || accept_loop(listener, sender, accept_stop))
            .map_err(|err| ControlError::Setup(err.to_string()))?;
        Ok(Self {
            dir,
            requests,
            stop,
        })
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.socket_path()
    }

    pub fn bin_dir(&self) -> PathBuf {
        self.dir.bin_dir()
    }

    pub(super) fn next(&self) -> Option<Incoming> {
        self.requests.try_recv().ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wakes the blocked accept so the thread sees the flag.
        let _ = UnixStream::connect(self.dir.socket_path());
    }
}

fn accept_loop(listener: UnixListener, requests: Sender<Incoming>, stop: Arc<AtomicBool>) {
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = stream else {
            continue;
        };
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _ = write_reply(
                &stream,
                &Reply::Error {
                    message: "Shika is serving too many commands at once.".into(),
                },
            );
            continue;
        }
        let requests = requests.clone();
        let active = active.clone();
        let spawned = std::thread::Builder::new()
            .name("shika-control-conn".into())
            .spawn({
                let active = active.clone();
                move || {
                    serve(stream, &requests);
                    active.fetch_sub(1, Ordering::SeqCst);
                }
            });
        if spawned.is_err() {
            active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// True when the client has gone away, so its reply would be lost.
fn peer_closed(stream: &UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return false;
    }
    let closed = matches!((&*stream).read(&mut [0u8; 1]), Ok(0));
    let _ = stream.set_nonblocking(false);
    closed
}

fn serve(stream: UnixStream, requests: &Sender<Incoming>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(10)));
    let mut delivered_to = None;
    let reply = match read_request(&stream) {
        Err(err) => Reply::Error {
            message: err.to_string(),
        },
        Ok(request) => match request.version_refusal() {
            Some(refusal) => refusal,
            None => {
                let (reply, answer) = mpsc::channel();
                let (confirm, delivered) = mpsc::channel();
                delivered_to = Some(confirm);
                let incoming = Incoming {
                    token: request.token,
                    command: request.command,
                    reply,
                    delivered,
                };
                if requests.send(incoming).is_err() {
                    Reply::Error {
                        message: "Shika is quitting.".into(),
                    }
                } else {
                    loop {
                        match answer.recv_timeout(Duration::from_millis(500)) {
                            Ok(reply) => break reply,
                            // Dropping `answer` tells the UI thread the
                            // client is gone, so a wait keeps its events.
                            Err(RecvTimeoutError::Timeout) if peer_closed(&stream) => return,
                            Err(RecvTimeoutError::Timeout) => {}
                            Err(RecvTimeoutError::Disconnected) => {
                                break Reply::Error {
                                    message: "Shika stopped handling this command. The Lead may have been closed."
                                        .into(),
                                };
                            }
                        }
                    }
                }
            }
        },
    };
    let written = write_reply(&stream, &reply).is_ok();
    if let Some(confirm) = delivered_to {
        let _ = confirm.send(written);
    }
}
