//! Session event pipeline.
//!
//! ACP callbacks run inside the protocol dispatch loop, so they must not do
//! slow work (disk writes, IPC). The pipeline moves events onto a dedicated
//! thread which coalesces streamed text chunks and forwards everything else in
//! order. Streamed deltas therefore never block the agent round-trip.

use crate::model::SessionEvent;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub type EventCallback = Arc<dyn Fn(SessionEvent) + Send + Sync + 'static>;

/// How long streamed text is allowed to accumulate before it is flushed.
const FLUSH_INTERVAL: Duration = Duration::from_millis(120);
/// Upper bound on a single accumulated text block.
const FLUSH_CHARS: usize = 8_192;

pub struct EventPipeline {
    tx: Option<Sender<Option<SessionEvent>>>,
    thread: Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl EventPipeline {
    pub fn new(callback: EventCallback) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Option<SessionEvent>>();
        let handle = std::thread::Builder::new()
            .name("lj-hub-events".into())
            .spawn(move || run(rx, callback))
            .ok();
        Self {
            tx: Some(tx),
            thread: Mutex::new(handle),
        }
    }

    /// Queue an event. Never blocks for longer than a channel send.
    pub fn emit(&self, event: SessionEvent) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Some(event));
        }
    }

    /// Flush pending text and stop the worker thread.
    pub fn close(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(None);
        }
        if let Ok(mut guard) = self.thread.lock() {
            if let Some(handle) = guard.take() {
                let _ = handle.join();
            }
        }
    }
}

#[derive(Default)]
struct Accum {
    stream: Option<String>,
    text: String,
}

impl Accum {
    fn push(&mut self, callback: &EventCallback, stream: &str, text: &str) {
        if self.stream.as_deref() != Some(stream) {
            self.flush(callback);
            self.stream = Some(stream.to_string());
        }
        self.text.push_str(text);
        if self.text.len() >= FLUSH_CHARS {
            self.flush(callback);
        }
    }

    fn flush(&mut self, callback: &EventCallback) {
        if let Some(stream) = self.stream.take() {
            if !self.text.is_empty() {
                callback(SessionEvent::Chunk {
                    stream,
                    text: std::mem::take(&mut self.text),
                });
            }
        }
        self.text.clear();
    }
}

fn run(rx: Receiver<Option<SessionEvent>>, callback: EventCallback) {
    let mut accum = Accum::default();
    loop {
        match rx.recv_timeout(FLUSH_INTERVAL) {
            Ok(Some(SessionEvent::Chunk { stream, text })) => {
                accum.push(&callback, &stream, &text);
            }
            Ok(Some(other)) => {
                // Ordering guarantee: pending text is delivered before any
                // structural event (tool call, permission, done, ...).
                accum.flush(&callback);
                callback(other);
            }
            Ok(None) => {
                accum.flush(&callback);
                return;
            }
            Err(RecvTimeoutError::Timeout) => accum.flush(&callback),
            Err(RecvTimeoutError::Disconnected) => {
                accum.flush(&callback);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    #[test]
    fn chunks_are_coalesced_and_ordered() {
        let (done_tx, done_rx) = channel::<Vec<String>>();
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        let pipeline = EventPipeline::new(Arc::new(move |event| {
            let mut guard = seen2.lock().unwrap();
            match event {
                SessionEvent::Chunk { stream, text } => {
                    guard.push(format!("chunk:{stream}:{text}"))
                }
                SessionEvent::PromptDone { stop_reason } => {
                    guard.push(format!("done:{stop_reason}"));
                    let _ = done_tx.send(guard.clone());
                }
                _ => {}
            }
        }));
        pipeline.emit(SessionEvent::Chunk {
            stream: "agent".into(),
            text: "Hel".into(),
        });
        pipeline.emit(SessionEvent::Chunk {
            stream: "agent".into(),
            text: "lo".into(),
        });
        pipeline.emit(SessionEvent::PromptDone {
            stop_reason: "end_turn".into(),
        });
        let seen = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        pipeline.close();
        assert_eq!(seen, vec!["chunk:agent:Hello", "done:end_turn"]);
    }
}
