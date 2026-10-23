//! Line reader and named pipe client shared by the core (server side helpers,
//! tests) and the UI (feature `ipc-client`, D-010).

use std::io;
use std::time::Duration;

use serde::de::DeserializeOwned;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};

use crate::{CoreToUi, UiToCore, decode_ipc_line, encode_ipc_line};

/// Longest accepted line (without counting more than this many bytes).
pub const MAX_LINE_BYTES: usize = 64 * 1024;
const ERROR_PIPE_BUSY: i32 = 231;

pub enum Line {
    Text(String),
    Eof,
    TooLong,
    /// Not valid UTF-8.
    Invalid,
}

/// Reads one line, never buffering more than [`MAX_LINE_BYTES`] + 1 bytes.
pub async fn read_line_limited<R: AsyncBufRead + Unpin>(r: &mut R) -> io::Result<Line> {
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

/// Minimal client: used by tests, `fake_ui` and the real UI.
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

    /// Splits into independent halves (the UI reads and writes from different tasks).
    pub fn into_halves(self) -> (IpcReader, IpcWriter) {
        (IpcReader { reader: self.reader }, IpcWriter { writer: self.writer })
    }

    pub async fn send(&mut self, msg: &UiToCore) -> io::Result<()> {
        send_msg(&mut self.writer, msg).await
    }

    /// Writes bytes as they are (tests use it to send malformed lines).
    pub async fn send_raw(&mut self, data: &str) -> io::Result<()> {
        send_raw(&mut self.writer, data).await
    }

    /// Next event; `Ok(None)` when the server closed the connection.
    pub async fn recv(&mut self) -> io::Result<Option<CoreToUi>> {
        recv_line(&mut self.reader).await
    }
}

/// Read half of an [`IpcClient`].
pub struct IpcReader {
    reader: BufReader<ReadHalf<NamedPipeClient>>,
}

impl IpcReader {
    pub async fn recv(&mut self) -> io::Result<Option<CoreToUi>> {
        recv_line(&mut self.reader).await
    }

    /// Next line as the server sent it (no parsing, so unknown variants pass through);
    /// `Ok(None)` when the server closed the connection.
    pub async fn recv_raw(&mut self) -> io::Result<Option<String>> {
        loop {
            match read_line_limited(&mut self.reader).await? {
                Line::Eof => return Ok(None),
                Line::TooLong | Line::Invalid => {
                    return Err(io::Error::new(io::ErrorKind::InvalidData, "invalid line from server"));
                }
                Line::Text(t) if t.trim().is_empty() => continue,
                Line::Text(t) => return Ok(Some(t)),
            }
        }
    }
}

/// Write half of an [`IpcClient`].
pub struct IpcWriter {
    writer: WriteHalf<NamedPipeClient>,
}

impl IpcWriter {
    pub async fn send(&mut self, msg: &UiToCore) -> io::Result<()> {
        send_msg(&mut self.writer, msg).await
    }

    pub async fn send_raw(&mut self, data: &str) -> io::Result<()> {
        send_raw(&mut self.writer, data).await
    }
}

async fn send_msg<W: AsyncWriteExt + Unpin>(w: &mut W, msg: &UiToCore) -> io::Result<()> {
    let line = encode_ipc_line(msg).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
    send_raw(w, &line).await
}

async fn send_raw<W: AsyncWriteExt + Unpin>(w: &mut W, data: &str) -> io::Result<()> {
    w.write_all(data.as_bytes()).await?;
    w.flush().await
}

async fn recv_line<R: AsyncBufRead + Unpin, T: DeserializeOwned>(r: &mut R) -> io::Result<Option<T>> {
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
