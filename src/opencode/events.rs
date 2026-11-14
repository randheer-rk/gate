//! SSE parsing and the single global OpenCode event subscription.
//!
//! `GET /api/event` is a process-wide firehose: it carries events for every
//! project and every session, including sessions Gate did not create. Gate
//! subscribes once and fans out in memory, filtering by session id.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use tokio::sync::broadcast;

use super::client::OpenCode;
use super::types::Event;
use crate::jobs::registry::Registry;

/// A parsed Server-Sent Event frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SseFrame {
    pub event: Option<String>,
    pub id: Option<String>,
    pub data: String,
}

#[derive(Debug, Default)]
pub struct SseParser {
    buffer: Vec<u8>,
    event: Option<String>,
    id: Option<String>,
    data: String,
}

impl SseParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed raw bytes; returns every complete frame.
    ///
    /// Chunk boundaries may split a UTF-8 sequence, so bytes are buffered and
    /// only decoded once a full line is available.
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseFrame> {
        self.buffer.extend_from_slice(chunk);
        let mut frames = Vec::new();

        while let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let mut line = String::from_utf8_lossy(&self.buffer[..index]).into_owned();
            self.buffer.drain(..=index);
            if line.ends_with('\r') {
                line.pop();
            }

            if line.is_empty() {
                if let Some(frame) = self.take_frame() {
                    frames.push(frame);
                }
                continue;
            }
            self.line(&line);
        }

        frames
    }

    /// Flush a trailing frame when the stream ends without a blank line.
    pub fn finish(&mut self) -> Option<SseFrame> {
        if !self.buffer.is_empty() {
            let mut tail = String::from_utf8_lossy(&self.buffer).into_owned();
            self.buffer.clear();
            if tail.ends_with('\r') {
                tail.pop();
            }
            if !tail.is_empty() {
                self.line(&tail);
            }
        }
        self.take_frame()
    }

    fn line(&mut self, line: &str) {
        // Comment / keep-alive, e.g. `: heartbeat`.
        if line.starts_with(':') {
            return;
        }

        let Some((field, value)) = line.split_once(':') else {
            return;
        };
        let value = value.strip_prefix(' ').unwrap_or(value);
        match field {
            "event" => self.event = Some(value.to_string()),
            "id" => self.id = Some(value.to_string()),
            "data" => {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(value);
            }
            _ => {}
        }
    }

    fn take_frame(&mut self) -> Option<SseFrame> {
        let data = std::mem::take(&mut self.data);
        let event = self.event.take();
        let id = self.id.take();
        if data.is_empty() {
            return None;
        }
        Some(SseFrame { event, id, data })
    }
}

/// Synthetic event published after a reconnect so consumers can resynchronise.
pub const RESYNC_EVENT: &str = "gate.bus.resync";

pub struct EventBus {
    tx: broadcast::Sender<Event>,
    generation: Arc<AtomicU64>,
}

impl EventBus {
    pub fn spawn(client: Arc<OpenCode>, registry: Arc<Registry>) -> Arc<Self> {
        let (tx, _) = broadcast::channel(256);
        let bus = Arc::new(Self {
            tx,
            generation: Arc::new(AtomicU64::new(0)),
        });
        tokio::spawn(run(bus.clone(), client, registry));
        bus
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.tx.subscribe()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

async fn run(bus: Arc<EventBus>, client: Arc<OpenCode>, registry: Arc<Registry>) {
    let mut backoff = Duration::from_secs(1);

    loop {
        match client.event_stream().await {
            Ok(mut stream) => {
                let generation = bus.generation.fetch_add(1, Ordering::Relaxed) + 1;
                tracing::info!(generation, "connected to OpenCode event stream");
                {
                    let _ = bus.tx.send(Event {
                        id: None,
                        created: None,
                        kind: RESYNC_EVENT.to_string(),
                        location: None,
                        data: serde_json::Value::Null,
                    });
                }

                let mut parser = SseParser::new();
                let mut received_any = false;

                while let Some(chunk) = stream.next().await {
                    match chunk {
                        Ok(bytes) => {
                            for frame in parser.push(&bytes) {
                                match serde_json::from_str::<Event>(&frame.data) {
                                    Ok(event) => {
                                        received_any = true;
                                        let Some(session) = event.session_id() else {
                                            continue;
                                        };
                                        let Some(job) = registry.by_session(session).await else {
                                            continue;
                                        };
                                        if event.directory() != Some(job.directory.as_str()) {
                                            continue;
                                        }
                                        let _ = bus.tx.send(event);
                                    }
                                    Err(error) => {
                                        tracing::debug!(
                                            %error,
                                            preview = %frame.data.chars().take(200).collect::<String>(),
                                            "dropping unparseable event"
                                        );
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "OpenCode event stream failed");
                            break;
                        }
                    }
                }

                if received_any {
                    backoff = Duration::from_secs(1);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not open OpenCode event stream");
            }
        }

        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_named_and_unnamed_events() {
        let mut parser = SseParser::new();
        let frames = parser.push(b"data: {\"a\":1}\n\n: heartbeat\n\ndata: {\"b\":2}\n\n");
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].data, "{\"a\":1}");
        assert_eq!(frames[0].event, None);
        assert_eq!(frames[1].data, "{\"b\":2}");
    }

    #[test]
    fn joins_multiline_data_and_reads_id() {
        let mut parser = SseParser::new();
        let frames = parser.push(b"id: 7\nevent: msg\ndata: line1\ndata: line2\n\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].id.as_deref(), Some("7"));
        assert_eq!(frames[0].event.as_deref(), Some("msg"));
        assert_eq!(frames[0].data, "line1\nline2");
    }

    #[test]
    fn handles_chunk_boundaries_and_crlf() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: {\"x\"").is_empty());
        let frames = parser.push(b":1}\r\n\r\n");
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].data, "{\"x\":1}");
    }

    #[test]
    fn flushes_trailing_frame_on_finish() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: tail").is_empty());
        let frame = parser.finish().expect("trailing frame");
        assert_eq!(frame.data, "tail");
    }

    #[test]
    fn ignores_heartbeat_only_chunks() {
        let mut parser = SseParser::new();
        assert!(parser.push(b": heartbeat\n\n").is_empty());
    }

    #[test]
    fn preserves_utf8_split_between_chunks() {
        let mut parser = SseParser::new();
        assert!(parser.push(b"data: h\xc3").is_empty());
        assert_eq!(parser.push(b"\xa9llo\n\n")[0].data, "héllo");
    }
}
