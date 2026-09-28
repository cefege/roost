//! The screen hub harness's recorders: a socket sink that records what the
//! hub asked of it, the replica owner's repair requests, and deadlines that
//! fire only when a test says so.
//!
//! Split out of `mod.rs` (the port of
//! `apps/coord/tests/terminal/screen/terminal-screen-hub-harness.ts`) for the
//! size cap.

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use roost_coord::sync_ws::retained_frame::SharedCellFrame;
use roost_coord::sync_ws::terminal::TerminalDeltaOutcome;
use roost_coord::sync_ws::terminal::snapshot::TerminalSnapshotSource;
use roost_coord::terminal_screen::ScreenReplicaSink;
use roost_coord::terminal_screen::hub_contract::{ScreenTimers, TerminalScreenSocketSink};
use roost_proto::{FirehoseFrame, PbCellGridFrame};
use roost_protocol::wire::SessionId;

use super::{SNAPSHOT_B, grid_of, texts};

/// One snapshot a socket was served, materialized at admission.
#[derive(Debug, Clone)]
pub struct Served {
    pub session_id: String,
    pub stream_id: String,
    pub parts: Vec<SharedCellFrame>,
}

impl Served {
    /// The single whole frame of an unchunked snapshot.
    pub fn frame(&self) -> PbCellGridFrame {
        assert_eq!(self.parts.len(), 1, "an unchunked snapshot is one frame");
        match &self.parts[0] {
            SharedCellFrame::Full(frame) => frame.clone(),
            SharedCellFrame::Chunk(_) => panic!("expected a whole frame, got a chunk"),
        }
    }
}

/// A callback a test runs from inside a sink, to re-enter the hub the way a
/// real socket's retirement can.
pub type Reentry = Arc<dyn Fn(&SessionId) + Send + Sync>;

/// A socket that records what the hub asked of it.
pub struct TestSink {
    pub events: Mutex<Vec<String>>,
    pub begins: Mutex<Vec<(String, String)>>,
    pub snapshots: Mutex<Vec<Served>>,
    pub deltas: Mutex<Vec<(String, String, PbCellGridFrame)>>,
    pub drops: Mutex<Vec<String>>,
    lanes: Mutex<HashMap<String, String>>,
    delta_outcome: Mutex<TerminalDeltaOutcome>,
    on_drop: Mutex<Option<Reentry>>,
    on_delta: Mutex<Option<Reentry>>,
}

impl TestSink {
    pub fn new(delta_outcome: TerminalDeltaOutcome) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Vec::new()),
            begins: Mutex::new(Vec::new()),
            snapshots: Mutex::new(Vec::new()),
            deltas: Mutex::new(Vec::new()),
            drops: Mutex::new(Vec::new()),
            lanes: Mutex::new(HashMap::new()),
            delta_outcome: Mutex::new(delta_outcome),
            on_drop: Mutex::new(None),
            on_delta: Mutex::new(None),
        })
    }

    pub fn queuing() -> Arc<Self> {
        Self::new(TerminalDeltaOutcome::Queued)
    }

    /// Run `reentry` after every recorded session drop.
    pub fn on_drop(&self, reentry: Reentry) {
        *self.on_drop.lock().unwrap() = Some(reentry);
    }

    /// Run `reentry` after every recorded delta.
    pub fn on_delta(&self, reentry: Reentry) {
        *self.on_delta.lock().unwrap() = Some(reentry);
    }

    pub fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    pub fn begins(&self) -> Vec<(String, String)> {
        self.begins.lock().unwrap().clone()
    }

    pub fn drops(&self) -> Vec<String> {
        self.drops.lock().unwrap().clone()
    }

    pub fn delta_texts(&self) -> Vec<Vec<String>> {
        self.deltas
            .lock()
            .unwrap()
            .iter()
            .map(|(_, _, frame)| texts(frame))
            .collect()
    }

    pub fn snapshots(&self) -> Vec<Served> {
        self.snapshots.lock().unwrap().clone()
    }

    pub fn last_seeded(&self) -> PbCellGridFrame {
        self.snapshots
            .lock()
            .unwrap()
            .last()
            .expect("a snapshot was served")
            .frame()
    }
}

impl TerminalScreenSocketSink for TestSink {
    fn begin_terminal_stream(&self, session_id: &SessionId, stream_id: &str) -> bool {
        let mut lanes = self.lanes.lock().unwrap();
        if lanes.get(session_id.as_str()).map(String::as_str) == Some(stream_id) {
            return false;
        }
        lanes.insert(session_id.as_str().to_owned(), stream_id.to_owned());
        self.events
            .lock()
            .unwrap()
            .push(format!("begin:{stream_id}"));
        self.begins
            .lock()
            .unwrap()
            .push((session_id.as_str().to_owned(), stream_id.to_owned()));
        true
    }

    fn replace_terminal_snapshot(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        source: Arc<dyn TerminalSnapshotSource>,
    ) -> bool {
        let Some(cursor) = source.create_cursor(SNAPSHOT_B) else {
            return false;
        };
        let parts = (0..cursor.part_count())
            .map(|index| {
                cursor
                    .materialize(index)
                    .expect("every planned part materializes")
            })
            .collect();
        self.events
            .lock()
            .unwrap()
            .push(format!("snapshot:{stream_id}"));
        self.snapshots.lock().unwrap().push(Served {
            session_id: session_id.as_str().to_owned(),
            stream_id: stream_id.to_owned(),
            parts,
        });
        true
    }

    fn enqueue_terminal_delta(
        &self,
        session_id: &SessionId,
        stream_id: &str,
        frame: &FirehoseFrame,
    ) -> TerminalDeltaOutcome {
        self.events
            .lock()
            .unwrap()
            .push(format!("delta:{stream_id}"));
        self.deltas.lock().unwrap().push((
            session_id.as_str().to_owned(),
            stream_id.to_owned(),
            grid_of(frame),
        ));
        let reentry = self.on_delta.lock().unwrap().clone();
        if let Some(reentry) = reentry {
            reentry(session_id);
        }
        *self.delta_outcome.lock().unwrap()
    }

    fn drop_terminal_session(&self, session_id: &SessionId) {
        self.lanes.lock().unwrap().remove(session_id.as_str());
        self.events
            .lock()
            .unwrap()
            .push(format!("drop:{}", session_id.as_str()));
        self.drops
            .lock()
            .unwrap()
            .push(session_id.as_str().to_owned());
        let reentry = self.on_drop.lock().unwrap().clone();
        if let Some(reentry) = reentry {
            reentry(session_id);
        }
    }
}

/// The replica owner's side: every repair request the hub made.
#[derive(Default)]
pub struct RecordingOwner {
    pub requests: Mutex<Vec<(String, String)>>,
    pub fresh_streams: Mutex<Vec<(String, String, String)>>,
    pub unavailable: Mutex<Vec<(String, String)>>,
    pub accepted: Mutex<Vec<(String, String)>>,
}

impl ScreenReplicaSink for RecordingOwner {
    fn request_snapshot(&self, session_id: &SessionId, stream_id: &str) {
        self.requests
            .lock()
            .unwrap()
            .push((session_id.as_str().to_owned(), stream_id.to_owned()));
    }

    fn request_fresh_stream(&self, session_id: &SessionId, expected_stream_id: &str, reason: &str) {
        let request = (
            session_id.as_str().to_owned(),
            expected_stream_id.to_owned(),
            reason.to_owned(),
        );
        self.fresh_streams.lock().unwrap().push(request);
    }

    fn unavailable(&self, session_id: &SessionId, reason: &str) {
        self.unavailable
            .lock()
            .unwrap()
            .push((session_id.as_str().to_owned(), reason.to_owned()));
    }

    fn full_accepted(&self, session_id: &SessionId, stream_id: &str) {
        self.accepted
            .lock()
            .unwrap()
            .push((session_id.as_str().to_owned(), stream_id.to_owned()));
    }
}

type Deadline = (u64, Box<dyn FnOnce() + Send>);

/// Deadlines that only fire when the test says so. The hub never cancels one:
/// a superseded deadline still fires and must find nothing to do, which is how
/// a test observes v2's `clearTimer`.
#[derive(Default)]
pub struct ManualTimers {
    next: AtomicU64,
    armed: Mutex<BTreeMap<u64, Deadline>>,
}

impl ScreenTimers for ManualTimers {
    fn schedule(&self, delay_ms: u64, fire: Box<dyn FnOnce() + Send>) {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.armed.lock().unwrap().insert(id, (delay_ms, fire));
    }
}

impl ManualTimers {
    /// Every armed deadline's id and delay, oldest first.
    pub fn armed(&self) -> Vec<(u64, u64)> {
        self.armed
            .lock()
            .unwrap()
            .iter()
            .map(|(id, (delay, _))| (*id, *delay))
            .collect()
    }

    /// The most recently armed deadline's id and delay.
    pub fn newest(&self) -> (u64, u64) {
        *self.armed().last().expect("a deadline is armed")
    }

    /// Fire one deadline.
    pub fn fire(&self, id: u64) {
        let (_, fire) = self
            .armed
            .lock()
            .unwrap()
            .remove(&id)
            .expect("the deadline is armed");
        fire();
    }

    /// Fire every deadline armed right now, in arming order.
    pub fn fire_all(&self) {
        let ids: Vec<u64> = self.armed.lock().unwrap().keys().copied().collect();
        for id in ids {
            self.fire(id);
        }
    }
}
