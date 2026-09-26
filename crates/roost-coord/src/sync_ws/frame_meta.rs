//! Which Sync v2 domain a frame belongs to, and which weighted lane orders it
//! against the other lanes' frames on the same socket.
//!
//! This is the ONE place a `FirehoseFrame` is turned into the metadata the
//! per-socket scheduler consumes. The feed that produces frames and the queue
//! that orders them are different modules, and a bus publisher that disagreed
//! with the queue about a frame's lane would deadlock a session silently, so
//! the classification lives once. v2 kept it in the feed's frame builder
//! (`sync-feed-frames.ts:38-165`); the split here is by concept, not by file:
//! that TS file also *builds* the payloads, which is the feed's work, while the
//! classification below is a property of the wire shape alone.
//!
//! THE LANE IS A STARVATION POLICY, NOT A PRIORITY HINT. `Cell` appears eight
//! times in [`WEIGHTED_LANES`] against one `Nonterminal`, because a session
//! that is streaming a busy terminal must never stop streaming for a worker
//! presence delta. A lane is therefore consulted round-robin with a cursor
//! rather than compared.

use roost_proto::SyncDomain;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::session_event_proto::Kind;

/// The ordering classes a Sync v2 frame can occupy on one socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FeedLane {
    /// Terminal cells, chunked baselines, and terminal view states. Fenced
    /// behind the session's `opened` announcement.
    Cell,
    /// Session lifecycle and other session-keyed metadata.
    Session,
    /// A retained seed that must precede the pre-ready live segment.
    Retained,
    /// Everything the per-domain registries own: workers, workspaces, tasks,
    /// MCP, pair requests, audit rows.
    Nonterminal,
    /// A control frame. Unsequenced, never queued, never windowed.
    #[default]
    Control,
}

/// The round-robin lane order, with the weights
/// (`sync-ws-v2-state.ts:71-76`). Seven cell slots, four session slots, two
/// retained slots and one nonterminal slot per pass: a terminal streaming at
/// full rate still gets roughly two thirds of the passes.
pub const WEIGHTED_LANES: [FeedLane; 15] = [
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Cell,
    FeedLane::Session,
    FeedLane::Session,
    FeedLane::Session,
    FeedLane::Session,
    FeedLane::Retained,
    FeedLane::Retained,
    FeedLane::Nonterminal,
];

/// What the session needs to know about one frame besides the frame itself.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncFrameMeta {
    /// The domain that owns this frame, or `None` for a control.
    pub domain: Option<SyncDomain>,
    /// The ordering lane.
    pub lane: FeedLane,
    /// The session this frame is about, when it is about one.
    pub session_id: Option<String>,
    /// Sessions this frame announces. Cells for a session stay fenced until the
    /// announcement has been delivered AND acknowledged.
    pub announces: Vec<String>,
    /// Sessions this frame closes. Their queued cells are dropped on delivery.
    pub closes: Vec<String>,
    /// A retained seed that must be inserted before the pre-ready live segment.
    pub before_buffered: bool,
    /// The terminal stream this frame belongs to, when the session scheduler
    /// owns the frame rather than the feed.
    pub terminal_stream_id: Option<String>,
    /// The snapshot part this frame is, for a chunked baseline.
    pub terminal_cursor_index: Option<u32>,
    /// A terminal view-state fence for the session's FIFO.
    pub terminal_state: bool,
    /// The baseline of a freshly attached session, which may pass other
    /// sessions' queued deltas but never their snapshots.
    pub attach_snapshot: bool,
}

impl SyncFrameMeta {
    /// A control frame's metadata: unsequenced and outside every queue.
    #[must_use]
    pub fn control() -> Self {
        Self::default()
    }

    /// Cell-lane metadata for one session.
    #[must_use]
    pub fn cell(session_id: &str) -> Self {
        Self {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Cell,
            session_id: Some(session_id.to_owned()),
            ..Self::default()
        }
    }
}

/// Whether a domain is hydrated only when the client asks for it.
///
/// Audit is the one domain a socket never subscribes to implicitly: it is a
/// high-volume, install-wide stream that most tabs never read, and holding a
/// socket open on it is how a coordinator's memory becomes a function of how
/// many tabs are open (`sync-ws-v2-state.ts:84-86`).
#[must_use]
pub fn is_lazy_domain(domain: SyncDomain) -> bool {
    domain == SyncDomain::Audit
}

/// Classify one outbound frame: its domain, its lane, its session, and the
/// lifecycle it asserts.
#[must_use]
pub fn frame_meta_for(frame: &Frame) -> SyncFrameMeta {
    match frame {
        Frame::CellGrid(grid) => SyncFrameMeta::cell(&grid.session_id),
        Frame::CellGridChunk(chunk) => SyncFrameMeta {
            domain: Some(SyncDomain::Terminal),
            lane: FeedLane::Cell,
            session_id: chunk.part.as_option().map(|part| part.session_id.clone()),
            ..SyncFrameMeta::default()
        },
        Frame::TerminalViewState(state) => SyncFrameMeta::cell(&state.session_id),
        Frame::Sessions(event) => {
            let mut meta = session_keyed_meta(None);
            read_legacy_session_payload(&event.payload_json, &mut meta);
            meta
        }
        Frame::SessionEvent(event) => event.kind.as_ref().map_or_else(SyncFrameMeta::control, session_event_meta),
        Frame::SessionPresence(presence) => session_keyed_meta(Some(&presence.session_id)),
        Frame::TerminalTitle(title) => session_keyed_meta(Some(&title.session_id)),
        Frame::LastActivity(activity) => session_keyed_meta(Some(&activity.session_id)),
        Frame::AgentStatus(status) => session_keyed_meta(Some(&status.session_id)),
        Frame::WorkerPresence(_) | Frame::WorkerRoutable(_) => domain_meta(SyncDomain::Workers),
        Frame::WorkspaceDelta(_) => domain_meta(SyncDomain::Workspaces),
        Frame::TaskDelta(_) => domain_meta(SyncDomain::Tasks),
        Frame::McpMsg(_) => domain_meta(SyncDomain::Mcp),
        Frame::PairRequestDelta(_) => domain_meta(SyncDomain::Pair),
        Frame::AuditRow(_) => domain_meta(SyncDomain::Audit),
        // Everything else is a v2 control, plus the frozen proto residue: the
        // oneof tag survives in the schema but nothing in this coordinator
        // produces the frame.
        Frame::UiState(_)
        | Frame::UiCommand(_)
        | Frame::Keepalive(_)
        | Frame::CoordinatorRelocation(_)
        | Frame::Subscribed(_)
        | Frame::DomainReset(_)
        | Frame::InputAccepted(_)
        | Frame::InputRejected(_)
        | Frame::InputAmbiguous(_)
        | Frame::InputRouteResult(_)
        | Frame::TerminalTransportProbeResult(_) => SyncFrameMeta::control(),
    }
}

fn domain_meta(domain: SyncDomain) -> SyncFrameMeta {
    SyncFrameMeta {
        domain: Some(domain),
        lane: FeedLane::Nonterminal,
        ..SyncFrameMeta::default()
    }
}

/// Session-keyed metadata: the terminal domain owns every session-keyed frame,
/// including presence, title, activity and agent status.
fn session_keyed_meta(session_id: Option<&str>) -> SyncFrameMeta {
    SyncFrameMeta {
        domain: Some(SyncDomain::Terminal),
        lane: FeedLane::Session,
        session_id: session_id.map(str::to_owned),
        ..SyncFrameMeta::default()
    }
}

fn session_event_meta(kind: &Kind) -> SyncFrameMeta {
    match kind {
        Kind::Opened(opened) => {
            let mut meta = session_keyed_meta(Some(&opened.session_id));
            meta.announces.push(opened.session_id.clone());
            meta
        }
        Kind::Closed(closed) => {
            let mut meta = session_keyed_meta(Some(&closed.session_id));
            meta.closes.push(closed.session_id.clone());
            meta
        }
        Kind::Snapshot(snapshot) => {
            let mut meta = session_keyed_meta(None);
            meta.announces =
                snapshot.sessions.iter().map(|row| row.id.clone()).collect();
            meta
        }
        _ => session_keyed_meta(kind_session_id(kind).as_deref()),
    }
}

fn kind_session_id(kind: &Kind) -> Option<String> {
    match kind {
        Kind::Attached(event) => Some(event.session_id.clone()),
        Kind::Detached(event) => Some(event.session_id.clone()),
        Kind::Cwd(event) => Some(event.session_id.clone()),
        Kind::WorkspaceAssigned(event) => Some(event.session_id.clone()),
        Kind::Respawned(event) => Some(event.session_id.clone()),
        Kind::Renamed(event) => Some(event.session_id.clone()),
        Kind::Git(event) => Some(event.session_id.clone()),
        Kind::Pr(event) => Some(event.session_id.clone()),
        Kind::Ports(event) => Some(event.session_id.clone()),
        Kind::AgentReference(event) => Some(event.session_id.clone()),
        Kind::Opened(_) | Kind::Closed(_) | Kind::Snapshot(_) => None,
    }
}

/// Read the routing fields out of a legacy JSON session event.
///
/// The typed `SessionEventProto` is the only frame shape this coordinator
/// emits, so this path exists for a frame that arrived in the pre-proto
/// fallback. It reads only the three fields the queue needs, and a payload that
/// is not the expected shape loses its ROUTING rather than its delivery: the
/// session lane is not the cell lane, so an unreadable payload cannot fence a
/// terminal (`sync-feed-frames.ts:70-88`).
fn read_legacy_session_payload(payload_json: &str, meta: &mut SyncFrameMeta) {
    let Ok(payload) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        tracing::debug!(
            event = "connect.sync.session_meta_parse_failed",
            dropped_fields = "session_id,announces,closes",
            "a JSON session event carried no readable routing fields"
        );
        return;
    };
    let Some(session_id) = payload.get("session_id").and_then(|id| id.as_str()) else {
        return;
    };
    meta.session_id = Some(session_id.to_owned());
    match payload.get("kind").and_then(serde_json::Value::as_str) {
        Some("opened") => meta.announces.push(session_id.to_owned()),
        Some("closed") => meta.closes.push(session_id.to_owned()),
        _ => {}
    }
}
