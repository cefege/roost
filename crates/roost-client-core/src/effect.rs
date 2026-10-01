//! What the client asks the host to do, in the order it asks it.
//!
//! Effects are TYPED, not encoded. The core decides that a resync is owed; the
//! host turns `SyncCommand::Resync` into bytes on whichever socket it holds. That
//! split is deliberate: the encoding is what changes when the protocol changes,
//! and a state machine that also owns its codec is a state machine whose tests
//! need a protobuf encoder.
//!
//! Effects in one return value are ORDERED. Two of them in one `handle` call are
//! a decision with a sequence — a dial before the subscribe that rides on it —
//! not a set, and a host that reorders them breaks the negotiation.

use crate::sync::link::{SyncDial, SyncDomain};
use crate::terminal::token::TerminalToken;
use crate::terminal::view::ViewIntent;

/// One thing the host should do, now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Open a Sync socket. The core names the contract; the host owns the origin.
    DialSync {
        /// Which dial this is. A host that fails to open it must still retire
        /// this generation, or the next dial's generation is off by one.
        generation: u64,
        /// The handshake, including the recovery cursor.
        dial: SyncDial,
    },
    /// Close the socket this generation owns, so the host's dial loop redials.
    CloseSyncLink {
        /// The generation to close.
        generation: u64,
        /// Why, for the host's own log. One of the immediate-redial reasons in
        /// `apps/web/src/client/sync/sync-flow.ts:72-78`, or a host-defined one.
        reason: String,
    },
    /// Send one typed client frame on the Sync socket.
    SendSync(SyncCommand),
    /// Send one typed command on a direct carrier.
    SendDirect {
        /// The exact carrier generation. A host that cannot match it must drop
        /// the command rather than send it on a route it has moved off.
        token: TerminalToken,
        /// The command.
        command: DirectCommand,
    },
    /// Make one Connect unary call.
    Rpc(RpcCall),
    /// Write the Sync recovery watermark. Debounced by the core, so a credential
    /// boundary can discard it before it reaches storage.
    PersistWatermark {
        /// The highest event id folded into the store.
        event_id: u64,
    },
    /// Write this profile's agent-status acknowledgement ledger.
    ///
    /// The host MERGES before it writes: two tabs on one profile acknowledge
    /// independently, and a blind write from whichever tab saved last would
    /// discard the other's. The encoded value is the `roost.agentSeen.v2` shape
    /// (`AgentSeenLedger::encode`), so the host's merge is a decode, a union, and
    /// an encode — no second format.
    PersistAgentSeen {
        /// The whole ledger, encoded.
        encoded: String,
    },
    /// Ask the coordinator for a time-bounded, memory-only direct-terminal grant
    /// naming exact sessions, one worker fingerprint, and this tab.
    ///
    /// Only a worker's acknowledgement reveals the secret, so a request that
    /// returns without one is a refusal, not a pending state.
    RequestDirectGrant {
        /// The session the grant is for.
        session_id: String,
        /// The worker whose loopback door or peer the grant opens.
        worker_fp: String,
    },
    /// A worker is gone: close every live direct connection it held.
    ///
    /// The core can retire a route because a route is a value in the store, but
    /// a socket is not — it belongs to the host. Without this the browser keeps
    /// a live carrier to a machine an operator deleted, and PTY input keeps
    /// being accepted until the grant's own TTL expires
    /// (`docs/FAILURE-INDEX.md` "A deleted worker's direct terminal still
    /// accepts input").
    CloseDirectCarriers {
        /// The worker whose connections go.
        worker_fp: String,
    },
    /// Perform one peer-lifecycle action the direct-carrier state machine
    /// decided on: open a transport, hand it the coordinator's answer, stage
    /// the candidate it authenticated, close the attempt, or come back later.
    ///
    /// `Box` because `CarrierEffect::Core` carries an `Effect`, and the two
    /// referencing each other directly is an infinitely sized type. `Core` is
    /// unwrapped by `client::carriers::lane`, so what a host sees here is only
    /// the arms that have no `Effect` spelling of their own.
    Carrier(Box<crate::client::carriers::CarrierEffect>),
    /// Ask the host to mint a view id with ITS OWN entropy, for a pane whose
    /// authority-facing id has to differ from the one it already holds.
    ///
    /// The core has no RNG and no JS: it names a session, an attempt and a pane,
    /// and the host answers with `ClientEvent::TerminalViewIdMinted`. Every
    /// reason the id must be a fresh UUID lives in the worker's admission (v2
    /// `validateTerminalViewCommand`), which is why the host mints it and the
    /// core refuses whatever comes back that is not one.
    MintTerminalViewId {
        /// The session the pane belongs to.
        session_id: String,
        /// The staging attempt this id is for, so an answer that arrives after
        /// the attempt moved on is recognisable as stale.
        attempt_id: u64,
        /// The pane's own identity, which never changes.
        logical_view_id: String,
        /// Which attempt is asking: a staged candidate's own view, or a
        /// re-registration on Sync after an elected direct route was lost.
        target: ViewIdTarget,
    },
}

/// Which attempt is asking the host for a view id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewIdTarget {
    /// A staged direct candidate, which is preparing its own view per pane so
    /// the worker never holds two live sockets on one id.
    Candidate,
    /// The Sync fallback rotation, which is replacing ids that belonged to a
    /// direct carrier that is gone.
    SyncFallback,
}

impl ViewIdTarget {
    /// A short name for the incident log.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::SyncFallback => "sync_fallback",
        }
    }
}

/// One typed frame for the Sync socket.
///
/// The frame's `socket_id` is NOT here: the host stamps the id of the socket it
/// sends on (`client::sync::encode::encode_sync_command`), exactly as v2's link
/// did (`apps/web/src/store/sync-domain-state.ts:83-98`). A command that named
/// its own socket could name one the host has already replaced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncCommand {
    /// The cumulative flow-control acknowledgement.
    ///
    /// CUMULATIVE, and only ever for a frame the client actually applied. An
    /// ACK above the last sequence the coordinator sent closes the socket with
    /// `1008`, and an ACK for a frame that was dropped rather than applied
    /// releases records the client never saw.
    Ack {
        /// The highest `delivery_seq` applied. Never `0`: a control frame has no
        /// window cost, so acknowledging one would release application records
        /// this client has not processed.
        ack_delivery_seq: u64,
    },
    /// Subscribe to one domain, exactly.
    Subscribe {
        /// The domain.
        domain: SyncDomain,
        /// The domain generation the subscribe answers. The coordinator ignores
        /// one for any other generation.
        generation: u64,
    },
    /// Unsubscribe from one domain, exactly.
    Unsubscribe {
        /// The domain.
        domain: SyncDomain,
        /// The domain generation being left.
        generation: u64,
    },
    /// Close one domain's snapshot/live gap, presenting the snapshot token.
    DomainReady {
        /// The domain.
        domain: SyncDomain,
        /// The domain generation the snapshot belongs to. A mismatch is ignored
        /// by the coordinator, which would leave the gap open.
        generation: u64,
        /// The one-time token from the bootstrap call.
        snapshot_token: Option<String>,
    },
    /// Publish, park, or remove a view.
    TerminalView {
        /// The session.
        session_id: String,
        /// The view.
        view_id: String,
        /// What the view wants.
        intent: ViewIntent,
        /// The view's intent revision. A new intent carries a higher one; a
        /// heartbeat or a redial replay repeats the same one with the same
        /// payload, which the coordinator treats as idempotent. A lower one, or
        /// the same one with a different payload, is refused
        /// (`crates/roost-coord/src/terminal_view/admit.rs`).
        revision: u64,
        /// The generation the command belongs to.
        token: TerminalToken,
    },
    /// Request a fresh complete baseline.
    TerminalResync {
        /// The session.
        session_id: String,
        /// The view whose geometry the baseline must match.
        view_id: String,
        /// The stream the replica is fenced to. Never empty: a replica with no
        /// expected stream has nothing to repair toward and sends no resync.
        stream_id: String,
        /// The canonical grid's epoch, or empty with no canonical.
        grid_epoch: String,
        /// The canonical grid's sequence, or `0` with no canonical.
        seq: u64,
        /// The generation the request belongs to.
        token: TerminalToken,
    },
    /// Write one admitted input batch.
    TerminalInput {
        /// The session.
        session_id: String,
        /// The view the keystroke came from, when it came from one.
        view_id: Option<String>,
        /// The client-allocated batch sequence, which the result is correlated by.
        input_seq: u64,
        /// The bytes, copied. The core owns the copy because the front end's
        /// buffer is not guaranteed to outlive the hold.
        bytes: Vec<u8>,
        /// The route epoch the worker acknowledged, or empty when the worker does
        /// not implement `terminal-input-route-v1`.
        input_route_epoch: String,
        /// The generation the write belongs to.
        token: TerminalToken,
    },
    /// Answer an acknowledged layout apply on the exact socket it named.
    UiApplyLayoutResult(crate::client::ui_state::LayoutApplyResult),
}

/// One typed command for a direct carrier.
///
/// Each command carries the values the wire messages require, not a subset the
/// host can recover. A host that reconstructed them from the store at send time
/// would put on the wire whatever the store said AFTER the event that produced
/// this effect — a second answer to "what was this command", from a different
/// copy, arriving later. That is the drift the Sync fences exist to prevent,
/// moved one layer out; so the effect states them and the host sends them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DirectCommand {
    /// Publish, park, or remove a view over the direct link.
    View {
        /// The session.
        session_id: String,
        /// The view.
        view_id: String,
        /// What the view wants.
        intent: ViewIntent,
        /// The view's geometry revision — what makes a replay of the same
        /// revision and payload idempotent across a Sync redial.
        revision: u64,
    },
    /// Request a fresh complete baseline over the direct link.
    Resync {
        /// The session.
        session_id: String,
        /// The view whose geometry the baseline must match.
        view_id: String,
        /// The stream the client believes it is reading.
        stream_id: String,
        /// The grid epoch that stream is on.
        grid_epoch: String,
        /// The canonical sequence the client holds, which is what the repair is
        /// measured against.
        seq: u64,
    },
    /// Write one admitted input batch over the direct link.
    Input {
        /// The session.
        session_id: String,
        /// The view the keystroke came from, when it came from one.
        view_id: Option<String>,
        /// The client-allocated batch sequence.
        input_seq: u64,
        /// The bytes.
        bytes: Vec<u8>,
        /// The route the client believes owns this channel's input, so a grant
        /// that moved fences the batch instead of writing it.
        input_route_epoch: String,
    },
}

mod rpc_call;
mod rpc_result;

pub use rpc_call::{RpcCall, hydration_call};
pub use rpc_result::RpcResult;
