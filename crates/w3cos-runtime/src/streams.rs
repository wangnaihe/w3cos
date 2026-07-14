//! W3C Streams API — ReadableStream + ReadableStreamDefaultReader
//!
//! Mirrors the WHATWG Streams Standard:
//! https://streams.spec.whatwg.org/
//!
//! A `ReadableStream` wraps any byte source (HTTP response body, file, PTY
//! output, etc.) and lets consumers pull chunks via a `ReadableStreamDefaultReader`.
//! The underlying source runs on a background thread and pushes chunks through
//! an `mpsc` channel — matching the browser's "push source" model.
//!
//! # Example — streaming an HTTP response body
//! ```ignore
//! let stream = ReadableStream::from_response_body(response_body_reader);
//! let reader = stream.get_reader();
//! while let Some(chunk) = reader.read() {
//!     process(chunk);
//! }
//! ```

use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::thread;

// ── Chunk ──────────────────────────────────────────────────────────────────

/// A single chunk of data from a `ReadableStream`.
/// Mirrors the browser's `{ value: Uint8Array | undefined, done: bool }`.
#[derive(Debug, Clone)]
pub enum ReadResult {
    /// A chunk of bytes — `{ value: bytes, done: false }`.
    Chunk(Vec<u8>),
    /// Stream is fully consumed — `{ value: undefined, done: true }`.
    Done,
    /// An error occurred in the underlying source.
    Error(String),
}

// ── Internal state ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadableStreamState {
    Readable,
    Closed,
    Errored,
}

struct StreamInner {
    rx: Mutex<mpsc::Receiver<ReadResult>>,
    state: Mutex<ReadableStreamState>,
    locked: AtomicBool,
}

// ── ReadableStream ─────────────────────────────────────────────────────────

/// W3C `ReadableStream` — a source of streaming byte data.
///
/// Cloning is cheap (`Arc` internally). Only one reader can be active at a
/// time (`locked` flag mirrors the spec's "locked to a reader" concept).
pub struct ReadableStream {
    inner: Arc<StreamInner>,
}

impl ReadableStream {
    /// Create a `ReadableStream` from any `Read` source.
    /// The source is consumed on a background thread in `chunk_size` byte chunks.
    pub fn from_reader<R>(source: R, chunk_size: usize) -> Self
    where
        R: Read + Send + 'static,
    {
        let (tx, rx) = mpsc::channel();
        thread::Builder::new()
            .name("w3cos-stream-reader".into())
            .spawn(move || pump_reader(source, chunk_size, tx))
            .expect("spawn stream reader");

        Self {
            inner: Arc::new(StreamInner {
                rx: Mutex::new(rx),
                state: Mutex::new(ReadableStreamState::Readable),
                locked: AtomicBool::new(false),
            }),
        }
    }

    /// Create a `ReadableStream` from a pre-built channel sender.
    /// Useful when the producer already runs on its own thread (e.g. WebSocket).
    pub fn from_channel(rx: mpsc::Receiver<ReadResult>) -> Self {
        Self {
            inner: Arc::new(StreamInner {
                rx: Mutex::new(rx),
                state: Mutex::new(ReadableStreamState::Readable),
                locked: AtomicBool::new(false),
            }),
        }
    }

    /// Create a `ReadableStream` from a static byte buffer (already-complete data).
    pub fn from_bytes(data: Vec<u8>) -> Self {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(ReadResult::Chunk(data));
        let _ = tx.send(ReadResult::Done);
        Self::from_channel(rx)
    }

    /// `ReadableStream.locked` — true when a reader holds the lock.
    pub fn locked(&self) -> bool {
        self.inner.locked.load(Ordering::SeqCst)
    }

    /// `ReadableStream.getReader()` — acquire the default reader.
    /// Panics if the stream is already locked (mirrors the spec's TypeError).
    pub fn get_reader(&self) -> ReadableStreamDefaultReader {
        assert!(
            !self.inner.locked.swap(true, Ordering::SeqCst),
            "ReadableStream is already locked to a reader"
        );
        ReadableStreamDefaultReader {
            inner: Arc::clone(&self.inner),
        }
    }

    /// `ReadableStream.cancel()` — signal the source to stop producing.
    /// Drains and discards any buffered chunks.
    pub fn cancel(&self) {
        *self.inner.state.lock().unwrap() = ReadableStreamState::Closed;
        if let Ok(rx) = self.inner.rx.lock() {
            while rx.try_recv().is_ok() {}
        }
    }

    pub fn state(&self) -> ReadableStreamState {
        *self.inner.state.lock().unwrap()
    }
}

impl Clone for ReadableStream {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

// ── ReadableStreamDefaultReader ────────────────────────────────────────────

/// W3C `ReadableStreamDefaultReader` — the consumer side of a `ReadableStream`.
///
/// Dropping the reader releases the stream lock automatically.
pub struct ReadableStreamDefaultReader {
    inner: Arc<StreamInner>,
}

impl ReadableStreamDefaultReader {
    /// `reader.read()` — blocking read of the next chunk.
    /// Returns `None` only if the channel is disconnected unexpectedly.
    pub fn read(&self) -> ReadResult {
        if *self.inner.state.lock().unwrap() == ReadableStreamState::Closed {
            return ReadResult::Done;
        }
        match self.inner.rx.lock().unwrap().recv() {
            Ok(result) => {
                if matches!(result, ReadResult::Done | ReadResult::Error(_)) {
                    *self.inner.state.lock().unwrap() = ReadableStreamState::Closed;
                }
                result
            }
            Err(_) => {
                *self.inner.state.lock().unwrap() = ReadableStreamState::Closed;
                ReadResult::Done
            }
        }
    }

    /// Non-blocking read — returns `None` if no chunk is available yet.
    pub fn try_read(&self) -> Option<ReadResult> {
        if *self.inner.state.lock().unwrap() == ReadableStreamState::Closed {
            return Some(ReadResult::Done);
        }
        match self.inner.rx.lock().unwrap().try_recv() {
            Ok(result) => {
                if matches!(result, ReadResult::Done | ReadResult::Error(_)) {
                    *self.inner.state.lock().unwrap() = ReadableStreamState::Closed;
                }
                Some(result)
            }
            Err(mpsc::TryRecvError::Empty) => None,
            Err(mpsc::TryRecvError::Disconnected) => {
                *self.inner.state.lock().unwrap() = ReadableStreamState::Closed;
                Some(ReadResult::Done)
            }
        }
    }

    /// Convenience: collect all chunks into a `Vec<u8>` (blocks until done).
    pub fn read_to_end(&self) -> Result<Vec<u8>, String> {
        let mut buf = Vec::new();
        loop {
            match self.read() {
                ReadResult::Chunk(chunk) => buf.extend_from_slice(&chunk),
                ReadResult::Done => return Ok(buf),
                ReadResult::Error(e) => return Err(e),
            }
        }
    }

    /// Convenience: collect all chunks as UTF-8 text (blocks until done).
    pub fn read_to_string(&self) -> Result<String, String> {
        let bytes = self.read_to_end()?;
        String::from_utf8(bytes).map_err(|e| format!("UTF-8 decode error: {e}"))
    }

    /// `reader.cancel()` — release the lock and cancel the stream.
    pub fn cancel(self) {
        // Drop releases the lock via Drop impl below.
        self.inner.cancel_inner();
    }
}

impl Drop for ReadableStreamDefaultReader {
    fn drop(&mut self) {
        self.inner.locked.store(false, Ordering::SeqCst);
    }
}

impl StreamInner {
    fn cancel_inner(&self) {
        *self.state.lock().unwrap() = ReadableStreamState::Closed;
        if let Ok(rx) = self.rx.lock() {
            while rx.try_recv().is_ok() {}
        }
        self.locked.store(false, Ordering::SeqCst);
    }
}

// ── Background pump ────────────────────────────────────────────────────────

fn pump_reader<R: Read>(mut source: R, chunk_size: usize, tx: mpsc::Sender<ReadResult>) {
    let mut buf = vec![0u8; chunk_size];
    loop {
        match source.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send(ReadResult::Done);
                break;
            }
            Ok(n) => {
                if tx.send(ReadResult::Chunk(buf[..n].to_vec())).is_err() {
                    break; // receiver dropped — stream was cancelled
                }
            }
            Err(e) => {
                let _ = tx.send(ReadResult::Error(e.to_string()));
                break;
            }
        }
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn read_all_chunks() {
        let data = b"hello world from w3cos streams";
        let stream = ReadableStream::from_reader(Cursor::new(data), 8);
        let reader = stream.get_reader();
        let result = reader.read_to_end().unwrap();
        assert_eq!(result, data);
    }

    #[test]
    fn from_bytes_convenience() {
        let stream = ReadableStream::from_bytes(b"abc".to_vec());
        let reader = stream.get_reader();
        assert_eq!(reader.read_to_string().unwrap(), "abc");
    }

    #[test]
    fn locked_flag() {
        let stream = ReadableStream::from_bytes(vec![]);
        assert!(!stream.locked());
        let reader = stream.get_reader();
        assert!(stream.locked());
        drop(reader);
        assert!(!stream.locked());
    }

    #[test]
    fn try_read_non_blocking() {
        let (tx, rx) = mpsc::channel();
        let stream = ReadableStream::from_channel(rx);
        let reader = stream.get_reader();
        assert!(reader.try_read().is_none());
        tx.send(ReadResult::Chunk(b"hi".to_vec())).unwrap();
        let chunk = reader.try_read().unwrap();
        assert!(matches!(chunk, ReadResult::Chunk(b) if b == b"hi"));
    }
}
