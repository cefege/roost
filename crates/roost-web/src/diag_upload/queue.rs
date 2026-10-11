//! A bounded, cloneable handoff from tracing callbacks to the browser uploader.
//!
//! The tracing layer only appends; the pump drains batches on its own timer.
//! Capacity is fixed so a disconnected coordinator cannot grow browser memory.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};

use roost_client_core::client::rpc::calls::diag_debug_log_batch::DiagDebugLogEntry;

const CAPACITY: usize = 512;

#[derive(Clone, Default, Debug)]
pub struct DiagUploadQueue {
    inner: Arc<Mutex<QueueState>>,
}

#[derive(Default, Debug)]
struct QueueState {
    entries: VecDeque<DiagDebugLogEntry>,
    dropped: u64,
}

impl DiagUploadQueue {
    pub fn push(&self, entry: DiagDebugLogEntry) {
        let mut state = self.lock();
        if state.entries.len() == CAPACITY {
            state.entries.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
        state.entries.push_back(entry);
    }

    pub fn is_empty(&self) -> bool {
        let state = self.lock();
        state.entries.is_empty() && state.dropped == 0
    }

    pub fn drain(&self, max: usize) -> Vec<DiagDebugLogEntry> {
        if max == 0 {
            return Vec::new();
        }
        let mut state = self.lock();
        let mut entries = Vec::with_capacity(max.min(state.entries.len()).saturating_add(1));
        if state.dropped != 0 {
            entries.push(dropped_entry(state.dropped));
            state.dropped = 0;
        }
        let take = max.saturating_sub(entries.len()).min(state.entries.len());
        entries.extend(state.entries.drain(..take));
        entries
    }

    pub fn requeue_front(&self, entries: Vec<DiagDebugLogEntry>) {
        let mut state = self.lock();
        for entry in entries.into_iter().rev() {
            state.entries.push_front(entry);
        }
        while state.entries.len() > CAPACITY {
            state.entries.pop_front();
            state.dropped = state.dropped.saturating_add(1);
        }
    }

    fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

fn dropped_entry(dropped: u64) -> DiagDebugLogEntry {
    DiagDebugLogEntry {
        evt: "spa diagnostic entries dropped".to_owned(),
        ts_ms: wall_time_ms(),
        mono_ns: monotonic_time_ns(),
        kv_json: serde_json::json!({
            "dropped": dropped,
            "target": "diag_upload",
            "level": "warn"
        })
        .to_string(),
        signal: true,
        ..Default::default()
    }
}

#[cfg(target_arch = "wasm32")]
fn wall_time_ms() -> u64 {
    js_sys::Date::now() as u64
}

#[cfg(not(target_arch = "wasm32"))]
fn wall_time_ms() -> u64 {
    0
}

#[cfg(target_arch = "wasm32")]
fn monotonic_time_ns() -> u64 {
    web_sys::window()
        .and_then(|window| window.performance())
        .map(|performance| (performance.now() * 1_000_000.0) as u64)
        .unwrap_or(0)
}

#[cfg(not(target_arch = "wasm32"))]
fn monotonic_time_ns() -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: usize) -> DiagDebugLogEntry {
        DiagDebugLogEntry {
            evt: id.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn overflow_drops_oldest_and_counts() {
        let queue = DiagUploadQueue::default();
        for id in 0..CAPACITY + 2 {
            queue.push(entry(id));
        }
        let drained = queue.drain(CAPACITY + 2);
        assert_eq!(
            drained.first().map(|entry| entry.evt.as_str()),
            Some("spa diagnostic entries dropped")
        );
        let dropped: serde_json::Value =
            serde_json::from_str(&drained[0].kv_json).expect("valid drop JSON");
        assert_eq!(dropped["dropped"], 2);
        assert_eq!(drained[1].evt, "2");
        assert_eq!(drained.last().map(|entry| entry.evt.as_str()), Some("513"));
    }

    #[test]
    fn synthetic_drop_entry_is_emitted_once_then_counter_resets() {
        let queue = DiagUploadQueue::default();
        for id in 0..CAPACITY + 1 {
            queue.push(entry(id));
        }
        assert_eq!(queue.drain(1)[0].evt, "spa diagnostic entries dropped");
        assert_eq!(queue.drain(1)[0].evt, "1");
    }

    #[test]
    fn requeue_front_remains_bounded_and_drops_oldest_entries() {
        let queue = DiagUploadQueue::default();
        for id in 0..CAPACITY {
            queue.push(entry(id));
        }
        queue.requeue_front(vec![entry(600), entry(601)]);
        let drained = queue.drain(CAPACITY);
        assert_eq!(drained[0].evt, "spa diagnostic entries dropped");
        assert_eq!(drained[1].evt, "0");
        assert_eq!(drained.last().map(|entry| entry.evt.as_str()), Some("510"));
        assert_eq!(drained.len(), CAPACITY);
        assert_eq!(queue.drain(1)[0].evt, "511");
    }
}
