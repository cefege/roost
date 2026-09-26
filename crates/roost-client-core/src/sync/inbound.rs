//! The typed frame vocabulary a Sync socket delivers, and the rule that governs
//! what happens to one: apply it, then acknowledge it.
//!
//! The host decodes protobuf; the core decides. That split is why this enum
//! exists rather than `roost_proto::FirehoseFrame` in the public API — the state
//! machine's inputs are named, and a frame kind nobody has a rule for is a
//! compile error here instead of a silently dropped `case`.
//!
//! `delivery_seq` is carried beside every frame by the host, not in here,
//! because it is a property of the transport sequence rather than of the frame:
//! it is `0` for a control frame, since controls never consume the
//! coordinator's application window (`protocol/spec/sync.md:29`).

use roost_proto::{PbCellGridChunk, PbCellGridFrame};
use roost_protocol::wire::SessionMap;

use crate::sessions::WireEvent;
use crate::sync::link::SyncDomain;
use crate::terminal::input::InputOutcome;

/// One decoded frame from the Sync socket.
/// Only `PartialEq`: the cell messages and the session rows it carries are
/// protobuf and wire types, neither of which is `Eq`. A test that needs to
/// compare two frames compares the fields it cares about.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncFrame {
    /// The v2 announcement: this socket's identity and every domain generation.
    Subscribed {
        /// The coordinator's identity for this socket.
        socket_id: String,
        /// The worker process epoch behind it.
        process_epoch: String,
        /// `(domain, generation, subscribed)`, as announced.
        domains: Vec<(SyncDomain, u64, bool)>,
    },
    /// A domain was reset by the coordinator; its retained snapshot is gone.
    DomainReset {
        /// Which domain.
        domain: SyncDomain,
        /// The generation the reset established.
        generation: u64,
        /// The coordinator's reason string, carried through to the log.
        reason: String,
    },
    /// The snapshot/live gap for a domain is closed.
    DomainReady {
        /// Which domain.
        domain: SyncDomain,
        /// The generation the ready frame belongs to. A `domain_ready` for a
        /// generation other than the current one is stale and is refused.
        generation: u64,
        /// The one-time snapshot token, required for the terminal domain.
        snapshot_token: Option<String>,
    },
    /// A session-plane event, already decoded to the shared wire shape.
    SessionEvent {
        /// The event. Folded by `roost_protocol::wire::fold_event` and never by
        /// a switch in this crate.
        event: WireEvent,
        /// The durable event id, for the recovery watermark.
        event_id: u64,
    },
    /// An authoritative full session set: bootstrap, or a re-hydration.
    ///
    /// Carries the shared `SessionMap` rather than a `BTreeMap<String, _>` the
    /// core would have to re-key: converting a keyed map into branded ids is a
    /// second parse of the same rows, and a row whose id fails the brand check
    /// would be dropped silently instead of refused loudly.
    SessionsSnapshot {
        /// The complete session rows.
        sessions: SessionMap,
    },
    /// One authoritative cell frame for a session replica.
    CellGrid {
        /// The session the frame belongs to.
        session_id: String,
        /// The wire frame, still a proto message: the shared chunk assembler
        /// speaks that type, and re-encoding it here would be a second codec.
        frame: PbCellGridFrame,
    },
    /// One part of a chunked baseline.
    CellGridChunk {
        /// The session the part belongs to.
        session_id: String,
        /// The wire part.
        chunk: PbCellGridChunk,
    },
    /// A generation-matched view-state result: the authority acknowledged a
    /// view, or refused it.
    ViewState {
        /// The session.
        session_id: String,
        /// The view the command named.
        view_id: String,
        /// The generation the acknowledgement belongs to.
        generation: u64,
        /// Whether the authority holds the view.
        accepted: bool,
    },
    /// A truthful terminal-write result for one admitted input batch.
    InputResult {
        /// The session the batch was for.
        session_id: String,
        /// The batch's own sequence, which the client allocated.
        input_seq: u64,
        /// Whether the write happened, was refused, or is unknown.
        outcome: InputOutcome,
        /// The domain generation the result belongs to.
        generation: u64,
    },
    /// A timestamp-only liveness frame.
    Keepalive,
    /// A frame kind this build has no rule for.
    ///
    /// Named rather than dropped: an unrecognised frame is still sequenced, so
    /// it still has to be acknowledged or the coordinator's window stops
    /// releasing. Carrying it as a variant is what makes that obligation
    /// visible instead of leaving it to whoever adds the next frame type.
    Unknown {
        /// The protobuf field number, for the incident log.
        field: u32,
    },
}

impl SyncFrame {
    /// The domain this frame belongs to, or `None` when it is not domain-bound.
    ///
    /// `None` means the frame rides whatever domain the host already
    /// established — it is not an exemption from the readiness gate, and
    /// `SyncState::may_apply` still consults the domain table for it.
    pub const fn domain(&self) -> Option<SyncDomain> {
        match self {
            Self::DomainReady { domain, .. } | Self::DomainReset { domain, .. } => Some(*domain),
            Self::CellGrid { .. } | Self::CellGridChunk { .. } => Some(SyncDomain::Terminal),
            Self::Subscribed { .. }
            | Self::SessionEvent { .. }
            | Self::SessionsSnapshot { .. }
            | Self::ViewState { .. }
            | Self::InputResult { .. }
            | Self::Keepalive
            | Self::Unknown { .. } => None,
        }
    }

    /// A short name for the incident log.
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::Subscribed { .. } => "subscribed",
            Self::DomainReset { .. } => "domain_reset",
            Self::DomainReady { .. } => "domain_ready",
            Self::SessionEvent { .. } => "session_event",
            Self::SessionsSnapshot { .. } => "sessions_snapshot",
            Self::CellGrid { .. } => "cell_grid",
            Self::CellGridChunk { .. } => "cell_grid_chunk",
            Self::ViewState { .. } => "view_state",
            Self::InputResult { .. } => "input_result",
            Self::Keepalive => "keepalive",
            Self::Unknown { .. } => "unknown",
        }
    }

    /// The session this frame names, for routing it to a replica.
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::CellGrid { session_id, .. }
            | Self::CellGridChunk { session_id, .. }
            | Self::ViewState { session_id, .. }
            | Self::InputResult { session_id, .. } => Some(session_id),
            _ => None,
        }
    }
}
