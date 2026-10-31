//! JSON-line framing over the Unix socket: the client's one request and one
//! reply with its read timeout, and the server's read and write of a line.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use super::protocol::{Command, Reply, Request};
use super::{ControlError, MAX_REQUEST_BYTES, Result};

fn io_error(err: &std::io::Error) -> ControlError {
    ControlError::Io(err.to_string())
}

/// Client side: connect to `socket`, send one request line, read one reply
/// line. Blocking; `wait` blocks here for up to its timeout, `new` until the
/// launch finishes, `pr` until publishing ends, and `close` until the author
/// decides.
pub fn send(socket: &Path, request: &Request) -> Result<Reply> {
    let mut stream = UnixStream::connect(socket).map_err(|err| {
        ControlError::Connect(format!(
            "Could not reach Shika at {} ({err}). Is the app running, and was this Lead started by it?",
            socket.display()
        ))
    })?;
    // `wait` is bounded by its own timeout. `new` is not bounded at all: it
    // replies once the worktree is prepared, which can take as long as the
    // project's `timeout-seconds`, and a client that gave up early would
    // leave the worker running with no one told its id. The server always
    // answers a live app (the launch result, or an error), and a quitting
    // app closes the socket, which ends the read with EOF. `pr` runs a chain
    // of git and gh commands, each bounded at 120 seconds but not the chain,
    // and a client that gave up could miss the URL of a PR that was created.
    // `close` waits for the author at a confirmation dialog. Both are
    // unbounded for the same reason: the server always answers.
    let patience = match &request.command {
        Command::Wait { timeout_secs, .. } => {
            Some(Duration::from_secs(timeout_secs.saturating_add(30)))
        }
        Command::New { .. } | Command::Pr { .. } | Command::Close { .. } => None,
        _ => Some(Duration::from_secs(60)),
    };
    stream
        .set_write_timeout(Some(Duration::from_secs(10)))
        .and_then(|_| stream.set_read_timeout(patience))
        .map_err(|err| io_error(&err))?;
    let mut line =
        serde_json::to_vec(request).map_err(|err| ControlError::Protocol(err.to_string()))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|err| io_error(&err))?;
    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|err| io_error(&err))?;
    if reply.trim().is_empty() {
        return Err(ControlError::Protocol(
            "Shika closed the connection without a reply.".into(),
        ));
    }
    serde_json::from_str(&reply).map_err(|err| ControlError::Protocol(err.to_string()))
}

/// Server side: read the one request line from a connection. A line over
/// [`MAX_REQUEST_BYTES`] is [`ControlError::TooLarge`].
pub fn read_request(stream: impl Read) -> Result<Request> {
    let mut line = Vec::new();
    BufReader::new(stream)
        .take(MAX_REQUEST_BYTES as u64 + 1)
        .read_until(b'\n', &mut line)
        .map_err(|err| io_error(&err))?;
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(ControlError::TooLarge);
    }
    if line.is_empty() {
        return Err(ControlError::Protocol("empty request".into()));
    }
    serde_json::from_slice(&line).map_err(|err| ControlError::Protocol(err.to_string()))
}

/// Server side: write the one reply line.
pub fn write_reply(mut stream: impl Write, reply: &Reply) -> Result<()> {
    let mut line =
        serde_json::to_vec(reply).map_err(|err| ControlError::Protocol(err.to_string()))?;
    line.push(b'\n');
    stream
        .write_all(&line)
        .and_then(|_| stream.flush())
        .map_err(|err| io_error(&err))
}
