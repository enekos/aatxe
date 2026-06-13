//! Event bus: stamp → persist → broadcast.
//!
//! Every event flows through [`EventBus::emit`], which assigns the
//! sequence number, appends one JSON line to the session's
//! `events.jsonl`, and fans out to live SSE subscribers. Persistence
//! before broadcast means a crash never loses an event a client saw.

use crate::events::{Envelope, UiEvent};
use anyhow::{Context, Result};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::broadcast;

pub struct EventBus {
    tx: broadcast::Sender<Envelope>,
    seq: AtomicU64,
    sink: Mutex<File>,
    path: PathBuf,
}

impl EventBus {
    pub fn new(jsonl_path: &Path) -> Result<Self> {
        if let Some(parent) = jsonl_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let sink = OpenOptions::new()
            .create(true)
            .append(true)
            .open(jsonl_path)
            .with_context(|| format!("opening {}", jsonl_path.display()))?;
        let (tx, _) = broadcast::channel(4096);
        Ok(Self {
            tx,
            seq: AtomicU64::new(0),
            sink: Mutex::new(sink),
            path: jsonl_path.to_path_buf(),
        })
    }

    /// Stamp, persist, broadcast. Returns the envelope for callers that
    /// need the assigned `seq`.
    pub fn emit(&self, event: UiEvent) -> Envelope {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let ts_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let env = Envelope { seq, ts_ms, event };
        if let Ok(line) = serde_json::to_string(&env) {
            if let Ok(mut f) = self.sink.lock() {
                let _ = writeln!(f, "{line}");
            }
        }
        // No receivers is fine — events still persist for replay.
        let _ = self.tx.send(env.clone());
        env
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.tx.subscribe()
    }

    /// Re-read everything emitted so far (for SSE catch-up).
    pub fn replay(&self) -> Vec<Envelope> {
        Self::read_jsonl(&self.path)
    }

    /// Parse a session JSONL, silently dropping malformed lines — a
    /// truncated tail (crash mid-write) must not poison the replay.
    pub fn read_jsonl(path: &Path) -> Vec<Envelope> {
        let Ok(f) = File::open(path) else {
            return Vec::new();
        };
        BufReader::new(f)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str(&l).ok())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::UiEvent;

    fn notice(msg: &str) -> UiEvent {
        UiEvent::Notice {
            message: msg.into(),
        }
    }

    #[test]
    fn emit_assigns_monotonic_seq_and_persists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let bus = EventBus::new(&path).unwrap();
        assert_eq!(bus.emit(notice("one")).seq, 1);
        assert_eq!(bus.emit(notice("two")).seq, 2);
        let replay = bus.replay();
        assert_eq!(replay.len(), 2);
        assert_eq!(replay[1].seq, 2);
    }

    #[test]
    fn subscribers_receive_emitted_events() {
        let dir = tempfile::tempdir().unwrap();
        let bus = EventBus::new(&dir.path().join("e.jsonl")).unwrap();
        let mut rx = bus.subscribe();
        bus.emit(notice("ping"));
        let got = rx.try_recv().unwrap();
        assert_eq!(got.seq, 1);
    }

    #[test]
    fn read_jsonl_skips_malformed_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let bus = EventBus::new(&path).unwrap();
        bus.emit(notice("good"));
        // Simulate a crash mid-write: garbage + truncated JSON.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "not json at all").unwrap();
        write!(f, "{{\"seq\":99,\"tsMs\":1,\"type\":\"noti").unwrap();
        let replay = EventBus::read_jsonl(&path);
        assert_eq!(replay.len(), 1);
    }

    #[test]
    fn read_jsonl_on_missing_file_is_empty() {
        assert!(EventBus::read_jsonl(Path::new("/nonexistent/x.jsonl")).is_empty());
    }
}
