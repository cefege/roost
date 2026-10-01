//! What one local socket observed, and the fixture that hands the owner one.
//!
//! Split out of the parent because the session layer's scripted answers and the
//! socket that records what the owner did to it are read at opposite ends of a
//! test: one is what the worker will SAY, the other is what the reader SEES.
//! Keeping them together made the file one concept longer than the cap, and the
//! two have no rule in common beyond the fixture that wires them together.

use super::*;

/// Everything one local socket observed, in the order the owner produced it,
/// so "state before cells" is checked as an order and not as two counters.
#[derive(Debug, Default)]
pub struct RecordedSocket {
    pub states: Mutex<Vec<TerminalViewStateFrame>>,
    pub order: Mutex<Vec<String>>,
    pub overflows: AtomicU64,
    pub expiries: AtomicU64,
}

impl RecordedSocket {
    pub fn states(&self) -> Vec<TerminalViewStateFrame> {
        held(&self.states).clone()
    }

    pub fn order(&self) -> Vec<String> {
        held(&self.order).clone()
    }

    /// Every stream generation the owner told this socket about, in order.
    pub fn accepted_stream_ids(&self) -> Vec<String> {
        self.states()
            .into_iter()
            .filter(|frame| frame.status.as_known() == Some(TerminalViewStatus::Accepted))
            .map(|frame| frame.stream_id)
            .collect()
    }
}

impl LocalViewTransport for RecordedSocket {
    fn send_view_state(&self, frame: TerminalViewStateFrame) {
        let status = frame.status.as_known();
        held(&self.order).push(format!("state:{}:{status:?}", frame.stream_id));
        held(&self.states).push(frame);
    }

    fn send_cell_frame(
        &self,
        _channel_id: ChannelId,
        frame: &CellGridFrame,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        held(&self.order).push(format!("cell:{}", frame.stream_id));
        CellSinkResult::Sent
    }

    fn send_snapshot_part(
        &self,
        _channel_id: ChannelId,
        _part: &CellGridSnapshotPart,
        _timings: FrameTimings,
    ) -> CellSinkResult {
        held(&self.order).push("chunk".to_owned());
        CellSinkResult::Sent
    }

    fn on_overflow(&self) {
        self.overflows.fetch_add(1, Ordering::SeqCst);
    }

    fn on_view_expired(&self) {
        self.expiries.fetch_add(1, Ordering::SeqCst);
    }
}

/// One owner over the scripted session layer and a real uplink.
pub struct Fixture {
    pub owner: Arc<TerminalViewOwner>,
    pub sessions: Arc<FakeSessions>,
    clock: Arc<ManualClock>,
    upstream: UplinkReceiver,
    pub relayed: Vec<(String, TerminalViewStateFrame)>,
    pub projections: Vec<WTerminalViewProjection>,
}

impl Fixture {
    pub fn new(outcomes: &[Outcome]) -> Self {
        let (uplink, upstream) = channel();
        let sessions = Arc::new(FakeSessions::scripted(outcomes));
        let clock = Arc::new(ManualClock(AtomicU64::new(START_MS)));
        let owner = TerminalViewOwner::new(TerminalViewOwnerDeps {
            sessions: Arc::clone(&sessions) as Arc<dyn ViewSessionPort>,
            uplink,
            clock: Arc::clone(&clock) as Arc<dyn EventClock>,
            runtime: tokio::runtime::Handle::current(),
        });
        Self {
            owner,
            sessions,
            clock,
            upstream,
            relayed: Vec::new(),
            projections: Vec::new(),
        }
    }

    pub fn advance(&self, ms: u64) {
        self.clock.0.fetch_add(ms, Ordering::SeqCst);
    }

    /// Let every spawned apply completion and projection flush run, then
    /// collect what reached the link.
    pub async fn settle(&mut self) {
        for _ in 0..64 {
            tokio::task::yield_now().await;
        }
        while let Some(frame) = self.upstream.try_recv() {
            match frame {
                CoordWorkerUpstream::TerminalViewState(state) => {
                    let frame = state.frame.as_option().cloned().unwrap_or_default();
                    self.relayed.push((state.socket_id, frame));
                }
                CoordWorkerUpstream::TerminalViewProjection(projection) => {
                    self.projections.push(projection)
                }
                other => panic!("unexpected upstream frame {}", other.kind()),
            }
        }
    }

    pub fn local_socket(&self, socket_id: &str, tab_id: &str) -> Arc<RecordedSocket> {
        self.local_socket_as(socket_id, tab_id, &device())
    }

    /// A local socket for one NAMED device. The viewer key is
    /// `${fingerprint}:${tabId}`, so two devices are two participants and a
    /// second device is a membership change rather than the first one
    /// re-announcing itself.
    pub fn local_socket_as(
        &self,
        socket_id: &str,
        tab_id: &str,
        device_fingerprint: &str,
    ) -> Arc<RecordedSocket> {
        let recorded = Arc::new(RecordedSocket::default());
        self.owner.register_local(LocalViewRegistration {
            socket_id: socket_id.to_owned(),
            device_fingerprint: device_fingerprint.to_owned(),
            tab_id: tab_id.to_owned(),
            allows_session: Arc::new(|session_id| session_id == SESSION),
            transport: Arc::clone(&recorded) as Arc<dyn LocalViewTransport>,
        });
        recorded
    }
}
