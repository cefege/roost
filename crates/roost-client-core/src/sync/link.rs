//! The Sync socket's identity: the generation, the domains, and the link state.
//!
//! One `SyncState` per client. A dial allocates a NEW socket generation, and
//! every callback, timer, terminal repair and latched resync that names an older
//! generation is inert — that is the whole reason generation exists. The
//! recovery cursor lives beside this in `watermark`.
//!
//! Ported from `apps/web/src/store/sync-link-state.ts`; the contract is
//! `protocol/spec/sync.md:23-31` and the reasons are in
//! `docs/phase4-client-contract.md` §7.

use std::collections::BTreeMap;

use roost_protocol::versioning::SYNC_QUERY_V2;
use roost_protocol::wire::sync_ws::{SYNC_AUTH_SUBPROTOCOL, SYNC_QUERY_FLOW_V1, SYNC_WS_PATH};

use crate::platform::KeyValueStore;
use crate::sync::watermark::RecoveryWatermark;
use crate::terminal::token::{TerminalToken, TerminalTransport};

pub use crate::sync::domain::DomainToken;

/// The storage key the Sync recovery watermark persists under.
///
/// Stable across releases on purpose: it is a resume cursor, so renaming it
/// turns every upgrade into a full re-hydration.
pub const SYNC_WATERMARK_KEY: &str = "roost.syncLastEventId";

/// Close code the coordinator uses when it rejects the credential. Terminal for
/// the socket: redialing would present the same rejected credential, so the
/// client stops and reports instead of looping.
pub const SYNC_AUTH_REVOKED_CLOSE_CODE: u16 = 4_001;
/// Close code the coordinator uses when backpressure ends the socket: the
/// application window was exceeded, so records are held rather than dropped and
/// the client is expected to redial and resume from its cursor.
///
/// The coordinator names the same value `SYNC_CONNECTION_REJECTION_CLOSE_CODE`
/// (`protocol/spec/sync.md:47`); 1013 is the RFC 6455 "try again later" code and
/// both ends mean the same thing by it.
pub const SYNC_BACKPRESSURE_CLOSE_CODE: u16 = 1_013;

/// Close code THIS client uses to end a socket whose generation is finished,
/// after a terminal liveness timeout.
///
/// Client-initiated on purpose: the coordinator cannot distinguish "this tab gave
/// up on a dead worker" from "this tab went away", and a 1013 from the other
/// side would read as coordinator backpressure and start a redial storm.
pub const SYNC_GENERATION_RETIRED_CLOSE_CODE: u16 = 4_000;

/// How many pre-hydration frames are retained before the oldest is dropped.
///
/// A retained frame is dispatched when hydration completes, un-gated by
/// generation, because the coordinator will not replay it. The bound stops a
/// hydration that never completes from growing the queue without limit; at this
/// size a full frame per pane is still inside the coordinator's 4 MiB
/// application window, and a dropped frame is recoverable by a resync where an
/// unbounded queue is not.
pub const SYNC_RETAINED_FRAME_MAX: usize = 512;

/// The v2 domains a client subscribes to.
///
/// The wire numbers are restated here rather than imported because the generated
/// `SyncDomain` is a wire encoding and this is a state machine. They are not a
/// second source of truth: `tests/core_without_a_browser.rs` parses
/// `protocol/proto/roost/v1/sync.proto` and fails if these drift from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SyncDomain {
    /// Terminal streams, view leases, and input results.
    Terminal,
    /// The worker registry and presence.
    Workers,
    /// Workspace metadata.
    Workspaces,
    /// Task metadata.
    Tasks,
    /// MCP relay traffic.
    Mcp,
    /// Pairing ceremony.
    Pair,
    /// Audit rows.
    Audit,
}

impl SyncDomain {
    /// Every domain, in the coordinator's own order.
    pub const ALL: [SyncDomain; 7] = [
        SyncDomain::Terminal,
        SyncDomain::Workers,
        SyncDomain::Workspaces,
        SyncDomain::Tasks,
        SyncDomain::Mcp,
        SyncDomain::Pair,
        SyncDomain::Audit,
    ];

    /// The protobuf enum value. Never renumbered: a coordinator and a client that
    /// disagree here do not degrade, they subscribe to the wrong domain.
    pub const fn wire_value(self) -> i32 {
        match self {
            Self::Terminal => 1,
            Self::Workers => 2,
            Self::Workspaces => 3,
            Self::Tasks => 4,
            Self::Mcp => 6,
            Self::Pair => 7,
            Self::Audit => 9,
        }
    }

    /// The protobuf enum name, for a host encoding the command.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Terminal => "SYNC_DOMAIN_TERMINAL",
            Self::Workers => "SYNC_DOMAIN_WORKERS",
            Self::Workspaces => "SYNC_DOMAIN_WORKSPACES",
            Self::Tasks => "SYNC_DOMAIN_TASKS",
            Self::Mcp => "SYNC_DOMAIN_MCP",
            Self::Pair => "SYNC_DOMAIN_PAIR",
            Self::Audit => "SYNC_DOMAIN_AUDIT",
        }
    }
}

/// Everything one socket handshake needs, named rather than rendered.
///
/// The core states the CONTRACT — path, subprotocol, negotiation values, tab,
/// and the recovery cursor — and the host turns it into a URL, because the
/// origin is the one thing the core genuinely does not know.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDial {
    /// Always `/ws/coord-sync`.
    pub path: &'static str,
    /// Always `roost-auth`; the credential is a later entry in the same list
    /// and never goes in the URL.
    pub subprotocol: &'static str,
    /// The flow-control value; `1` selects the negotiated application window.
    pub flow: &'static str,
    /// The v2 domain-generation value.
    pub sync_v: &'static str,
    /// This tab's identity. A v2 socket without one is read-only
    /// (`protocol/spec/sync.md:25`).
    pub tab_id: String,
    /// The highest event id this tab has folded, for durable backfill.
    pub since: u64,
}

impl SyncDial {
    /// The dial this client makes, with its own cursor as the recovery point.
    pub fn for_tab(tab_id: impl Into<String>, since: u64) -> Self {
        Self {
            path: SYNC_WS_PATH,
            subprotocol: SYNC_AUTH_SUBPROTOCOL,
            flow: SYNC_QUERY_FLOW_V1,
            sync_v: SYNC_QUERY_V2,
            tab_id: tab_id.into(),
            since,
        }
    }
}

/// The current, authenticated, writable socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncLink {
    /// Which dial this is. Every callback carries it back to prove it is current.
    pub generation: u64,
    /// The coordinator's identity for this socket. Sent with every cumulative
    /// ACK, because the coordinator releases queued records per socket.
    pub socket_id: String,
    /// The worker process epoch behind this socket, which is part of the
    /// terminal generation token.
    pub process_epoch: String,
    /// The socket is open and the client may send.
    pub open: bool,
    /// The link still accepts frames — it has not been closed intentionally.
    pub accepting: bool,
    /// When the last frame of any kind arrived, for the liveness read.
    pub last_frame_at_ms: u64,
}

/// One frame held until hydration completes.
///
/// `delivery_seq` is carried so the cumulative ACK can still be sent: the frame
/// is applied later, but it was already sequenced when it arrived.
/// Only `PartialEq`: it carries a `SyncFrame`, which carries wire types.
#[derive(Debug, Clone, PartialEq)]
pub struct RetainedFrame {
    /// The frame itself, as the host decoded it.
    pub frame: crate::sync::inbound::SyncFrame,
    /// Its sequence, or `0` for a control frame.
    pub delivery_seq: u64,
    /// The socket generation it arrived on. Deliberately NOT a dispatch gate —
    /// see `docs/phase4-client-contract.md` §7.
    pub generation: u64,
}

/// The Sync state machine's whole state.
#[derive(Debug)]
pub struct SyncState {
    /// The generation the next dial will take. Monotonic, never reused.
    next_generation: u64,
    /// The live socket, if one is open.
    pub(crate) link: Option<SyncLink>,
    /// Per-domain hydration state for the live socket.
    pub(crate) domains: BTreeMap<SyncDomain, DomainToken>,
    /// The recovery cursor.
    pub(crate) watermark: RecoveryWatermark,
    /// Frames received before the store was hydrated, oldest first.
    retained: Vec<RetainedFrame>,
    /// The coordinator rejected the credential; no further dial is automatic.
    pub auth_revoked: bool,
}

impl SyncState {
    /// A client that has never dialled. The cursor is read from storage, so a
    /// reload resumes a backfill instead of re-hydrating everything.
    pub fn new(storage: &dyn KeyValueStore) -> Self {
        Self {
            next_generation: 1,
            link: None,
            domains: BTreeMap::new(),
            watermark: RecoveryWatermark::from_storage(storage),
            retained: Vec::new(),
            auth_revoked: false,
        }
    }

    /// The dial this client should make now, and the generation it will take.
    ///
    /// The generation is allocated HERE rather than when the socket opens, so a
    /// caller that starts work against it can already be fenced: a frame naming
    /// a generation nothing has accepted is inert from the moment it exists.
    pub fn begin_dial(&mut self, tab_id: &str) -> (u64, SyncDial) {
        let generation = self.next_generation;
        self.next_generation += 1;
        (
            generation,
            SyncDial::for_tab(tab_id, self.watermark.last_seen),
        )
    }

    /// How many sockets this client has dialled. A diagnostic, not a gate.
    pub fn dial_count(&self) -> u64 {
        self.next_generation - 1
    }

    /// The socket named by `generation` is open and owns the coordinator's scope.
    pub fn open_link(
        &mut self,
        generation: u64,
        socket_id: impl Into<String>,
        process_epoch: impl Into<String>,
        now_ms: u64,
    ) -> bool {
        if generation + 1 != self.next_generation || self.link.is_some() {
            return false;
        }
        self.link = Some(SyncLink {
            generation,
            socket_id: socket_id.into(),
            process_epoch: process_epoch.into(),
            open: true,
            accepting: true,
            last_frame_at_ms: now_ms,
        });
        self.domains.clear();
        self.retained.clear();
        true
    }

    /// Close the live socket, if it is still the one `generation` names.
    ///
    /// A close code of `SYNC_AUTH_REVOKED_CLOSE_CODE` latches `auth_revoked`,
    /// because redialing would present the same rejected credential in a loop.
    pub fn close_link(&mut self, generation: u64, close_code: Option<u16>) -> bool {
        if self.link.as_ref().map(|link| link.generation) != Some(generation) {
            return false;
        }
        self.link = None;
        self.domains.clear();
        if close_code == Some(SYNC_AUTH_REVOKED_CLOSE_CODE) {
            self.auth_revoked = true;
        }
        true
    }

    /// True when `generation` still names the current, open, accepting socket.
    ///
    /// This is the v2 `canAcceptSyncLink`
    /// (`apps/web/src/client/sync/sync-flow.ts:18-26`): same link, accepting, and
    /// open. Every cumulative ACK and every Sync-bound terminal command is gated
    /// on it.
    pub fn accepts(&self, generation: u64) -> bool {
        self.link
            .as_ref()
            .is_some_and(|link| link.generation == generation && link.accepting && link.open)
    }

    /// Close the live socket deliberately, so the existing dial loop redials.
    pub fn request_redial(&mut self, generation: u64) -> bool {
        if !self.accepts(generation) {
            return false;
        }
        if let Some(link) = self.link.as_mut() {
            link.accepting = false;
            link.open = false;
        }
        self.link = None;
        self.domains.clear();
        true
    }

    /// The current socket's id, for a cumulative ACK.
    pub fn socket_id(&self) -> Option<&str> {
        self.link.as_ref().map(|link| link.socket_id.as_str())
    }

    /// The current socket's generation.
    pub fn link_generation(&self) -> Option<u64> {
        self.link.as_ref().map(|link| link.generation)
    }

    /// Note that a frame of any kind arrived, for the liveness read.
    pub fn note_frame(&mut self, generation: u64, now_ms: u64) -> bool {
        match self.link.as_mut() {
            Some(link) if link.generation == generation => {
                link.last_frame_at_ms = now_ms;
                true
            }
            _ => false,
        }
    }

    /// Milliseconds since the live socket last received a frame, or `None`.
    pub fn idle_ms(&self, now_ms: u64) -> Option<u64> {
        self.link
            .as_ref()
            .map(|link| now_ms.saturating_sub(link.last_frame_at_ms))
    }

    /// The terminal generation token for the live socket, or `None`.
    ///
    /// Sync tokens carry no worker, so this can never be confused with a
    /// direct-carrier route.
    pub fn terminal_token(&self) -> Option<TerminalToken> {
        let link = self.link.as_ref()?;
        Some(TerminalToken::sync(
            link.generation,
            link.socket_id.clone(),
            link.process_epoch.clone(),
            self.domain_generation(SyncDomain::Terminal).unwrap_or(0),
        ))
    }

    /// The token a direct carrier carries: the same socket and domain generation,
    /// plus the worker it belongs to.
    pub fn direct_token(
        &self,
        worker_fp: impl Into<String>,
        transport: TerminalTransport,
    ) -> Option<TerminalToken> {
        let link = self.link.as_ref()?;
        Some(TerminalToken::direct(
            link.generation,
            transport,
            worker_fp,
            link.process_epoch.clone(),
            self.domain_generation(SyncDomain::Terminal).unwrap_or(0),
        ))
    }

    /// Hold a frame that arrived before the store was hydrated.
    pub fn retain(&mut self, frame: RetainedFrame) {
        if self.retained.len() >= SYNC_RETAINED_FRAME_MAX {
            self.retained.remove(0);
        }
        self.retained.push(frame);
    }

    /// How many frames are held.
    pub fn retained_len(&self) -> usize {
        self.retained.len()
    }

    /// Take every held frame, in arrival order.
    pub fn take_retained(&mut self) -> Vec<RetainedFrame> {
        std::mem::take(&mut self.retained)
    }
}
