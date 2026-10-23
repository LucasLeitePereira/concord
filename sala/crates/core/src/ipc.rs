//! IPC between the core and the UI over a Windows named pipe (D-010, D-001).
//!
//! One JSON message per line. The server accepts several clients; commands
//! (`UiToCore`) arrive on a single channel tagged with the `ClientId`, events
//! (`CoreToUi`) go to one client (`send_to`) or all (`broadcast`).
//! Message contents (chat!) are never logged (`inv-chat-never-on-disk`).

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sala_protocol::{CoreToUi, UiToCore, decode_ipc_line, encode_ipc_line};
use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tracing::{debug, warn};

/// Longest accepted line (without counting more than this many bytes).
pub const MAX_LINE_BYTES: usize = 64 * 1024;
const INPUT_QUEUE: usize = 256;
const ERROR_PIPE_BUSY: i32 = 231;

pub type ClientId = u64;

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("another core already owns the pipe")]
    AlreadyRunning,
    #[error("ipc i/o error: {0}")]
    Io(#[from] io::Error),
}

/// Replaces anything that is not `[A-Za-z0-9_.-]` with `_`; never returns empty.
fn sanitize(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') { c } else { '_' })
        .collect();
    if out.is_empty() { "user".to_string() } else { out }
}

/// `\\.\pipe\sala-core-<user>` or `\\.\pipe\sala-core-<user>-<profile>` (D-010).
pub fn pipe_name(user: &str, profile: Option<&str>) -> String {
    match profile {
        Some(p) => format!(r"\\.\pipe\sala-core-{}-{}", sanitize(user), sanitize(p)),
        None => format!(r"\\.\pipe\sala-core-{}", sanitize(user)),
    }
}

/// [`pipe_name`] for the current Windows user (`USERNAME`).
pub fn current_user_pipe_name(profile: Option<&str>) -> String {
    let user = std::env::var("USERNAME").unwrap_or_default();
    pipe_name(&user, profile)
}

enum Line {
    Text(String),
    Eof,
    TooLong,
    /// Not valid UTF-8.
    Invalid,
}

/// Reads one line, never buffering more than [`MAX_LINE_BYTES`] + 1 bytes.
async fn read_line_limited<R: tokio::io::AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Line> {
    let mut buf = Vec::new();
    let n = (&mut *r).take(MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut buf).await?;
    if n == 0 {
        return Ok(Line::Eof);
    }
    if buf.last() != Some(&b'\n') {
        // Either the limit was hit or the peer closed mid-line.
        return Ok(if buf.len() > MAX_LINE_BYTES { Line::TooLong } else { Line::Eof });
    }
    if buf.len() > MAX_LINE_BYTES + 1 {
        return Ok(Line::TooLong);
    }
    Ok(match String::from_utf8(buf) {
        Ok(s) => Line::Text(s),
        Err(_) => Line::Invalid,
    })
}

async fn write_loop<W: tokio::io::AsyncWrite + Unpin>(mut w: W, mut rx: mpsc::UnboundedReceiver<String>) {
    while let Some(line) = rx.recv().await {
        if w.write_all(line.as_bytes()).await.is_err() || w.flush().await.is_err() {
            break;
        }
    }
}

type ClientMap = Mutex<HashMap<ClientId, mpsc::UnboundedSender<String>>>;

/// Named pipe server owned by the core.
pub struct IpcServer {
    clients: Arc<ClientMap>,
    stop: watch::Sender<bool>,
    accept: Option<JoinHandle<()>>,
}

impl IpcServer {
    /// Creates the first pipe instance (fails with [`IpcError::AlreadyRunning`]
    /// if another core owns `name`) and starts accepting clients.
    /// Returns the server and the stream of `(client, command)`.
    /// Must be called inside a tokio runtime.
    pub fn start(name: &str) -> Result<(IpcServer, mpsc::Receiver<(ClientId, UiToCore)>), IpcError> {
        let first = create_instance(name, true).map_err(|e| {
            if e.kind() == io::ErrorKind::PermissionDenied { IpcError::AlreadyRunning } else { IpcError::Io(e) }
        })?;
        let clients: Arc<ClientMap> = Arc::new(Mutex::new(HashMap::new()));
        let (stop, stop_rx) = watch::channel(false);
        let (input_tx, input_rx) = mpsc::channel(INPUT_QUEUE);
        let accept = tokio::spawn(accept_loop(first, name.to_string(), clients.clone(), input_tx, stop_rx));
        Ok((IpcServer { clients, stop, accept: Some(accept) }, input_rx))
    }

    /// Sends to one client; false if it is gone or the message cannot be encoded.
    pub fn send_to(&self, client: ClientId, msg: &CoreToUi) -> bool {
        let Ok(line) = encode_ipc_line(msg) else { return false };
        match self.clients.lock().unwrap().get(&client) {
            Some(tx) => tx.send(line).is_ok(),
            None => false,
        }
    }

    /// Sends to every connected client; returns how many received it.
    pub fn broadcast(&self, msg: &CoreToUi) -> usize {
        let Ok(line) = encode_ipc_line(msg) else { return 0 };
        self.clients.lock().unwrap().values().filter(|tx| tx.send(line.clone()).is_ok()).count()
    }

    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Stops accepting, disconnects everyone (pending output is flushed first).
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(h) = self.accept.take() {
            let _ = h.await;
        }
        self.clients.lock().unwrap().clear();
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
    }
}

fn create_instance(name: &str, first: bool) -> io::Result<NamedPipeServer> {
    ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true).create(name)
}

async fn accept_loop(
    mut pipe: NamedPipeServer,
    name: String,
    clients: Arc<ClientMap>,
    input: mpsc::Sender<(ClientId, UiToCore)>,
    mut stop: watch::Receiver<bool>,
) {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            r = pipe.connect() => {
                if let Err(e) = r {
                    warn!("ipc accept failed: {e}");
                    break;
                }
            }
        }
        let next = match create_instance(&name, false) {
            Ok(p) => p,
            Err(e) => {
                warn!("ipc could not create next pipe instance: {e}");
                break;
            }
        };
        let connected = std::mem::replace(&mut pipe, next);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        tokio::spawn(handle_client(connected, id, clients.clone(), input.clone(), stop.clone()));
    }
}

async fn handle_client(
    pipe: NamedPipeServer,
    id: ClientId,
    clients: Arc<ClientMap>,
    input: mpsc::Sender<(ClientId, UiToCore)>,
    mut stop: watch::Receiver<bool>,
) {
    debug!(client = id, "ipc client connected");
    let (r, w) = tokio::io::split(pipe);
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    clients.lock().unwrap().insert(id, tx.clone());
    let writer = tokio::spawn(write_loop(w, rx));
    let mut reader = BufReader::new(r);
    let invalid = |tx: &mpsc::UnboundedSender<String>| {
        let err = CoreToUi::Error { message: "invalid message".to_string() };
        if let Ok(l) = encode_ipc_line(&err) {
            let _ = tx.send(l);
        }
    };
    loop {
        let line = tokio::select! {
            _ = stop.changed() => break,
            l = read_line_limited(&mut reader) => l,
        };
        match line {
            Ok(Line::Text(text)) => {
                if text.trim().is_empty() {
                    continue;
                }
                match decode_ipc_line::<UiToCore>(&text) {
                    Ok(cmd) => {
                        if input.send((id, cmd)).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => invalid(&tx),
                }
            }
            Ok(Line::Invalid) => invalid(&tx),
            Ok(Line::TooLong) => {
                warn!(client = id, "ipc line too long; disconnecting client");
                break;
            }
            Ok(Line::Eof) | Err(_) => break,
        }
    }
    clients.lock().unwrap().remove(&id);
    drop(tx);
    let _ = writer.await;
    debug!(client = id, "ipc client disconnected");
}

/// Minimal client: used by tests, `fake_ui` and (later) the real UI.
pub struct IpcClient {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
    writer: WriteHalf<NamedPipeClient>,
}

impl IpcClient {
    /// Connects to the pipe, retrying briefly while the server is between instances.
    pub async fn connect(name: &str) -> io::Result<IpcClient> {
        let mut attempts = 0;
        let pipe = loop {
            match ClientOptions::new().open(name) {
                Ok(p) => break p,
                Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 50 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        };
        let (r, w) = tokio::io::split(pipe);
        Ok(IpcClient { reader: BufReader::new(r), writer: w })
    }

    pub async fn send(&mut self, msg: &UiToCore) -> io::Result<()> {
        let line = encode_ipc_line(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        self.send_raw(&line).await
    }

    /// Writes bytes as they are (tests use it to send malformed lines).
    pub async fn send_raw(&mut self, data: &str) -> io::Result<()> {
        self.writer.write_all(data.as_bytes()).await?;
        self.writer.flush().await
    }

    /// Next event; `Ok(None)` when the server closed the connection.
    pub async fn recv(&mut self) -> io::Result<Option<CoreToUi>> {
        recv_line(&mut self.reader).await
    }
}

async fn recv_line<R: tokio::io::AsyncBufRead + Unpin, T: DeserializeOwned>(
    r: &mut R,
) -> io::Result<Option<T>> {
    loop {
        match read_line_limited(r).await? {
            Line::Eof => return Ok(None),
            Line::TooLong | Line::Invalid => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid line from server"));
            }
            Line::Text(t) if t.trim().is_empty() => continue,
            Line::Text(t) => {
                return decode_ipc_line(&t)
                    .map(Some)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_name_format_and_sanitize() {
        assert_eq!(pipe_name("ana", None), r"\\.\pipe\sala-core-ana");
        assert_eq!(pipe_name("ana", Some("b")), r"\\.\pipe\sala-core-ana-b");
        assert_eq!(pipe_name(r"DOM\ana smith", None), r"\\.\pipe\sala-core-DOM_ana_smith");
        assert_eq!(pipe_name("", Some("")), r"\\.\pipe\sala-core-user-user");
    }

    #[test]
    fn sanitize_blocks_path_tricks() {
        assert!(!pipe_name(r"..\..\x", None).trim_start_matches(r"\\.\pipe\").contains('\\'));
    }
}
