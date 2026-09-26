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
    /// Ask the host to sign a challenge with the device key.
    ///
    /// The result comes back as `ClientEvent::ChallengeSigned`. Async in every
    /// host, which is exactly why it is an effect and not a trait method.
    SignChallenge {
        /// What is being signed.
        purpose: ChallengePurpose,
        /// The bytes to sign, already canonicalised by the host.
        payload: Vec<u8>,
    },
    /// Write the Sync recovery watermark. Debounced by the core, so a credential
    /// boundary can discard it before it reaches storage.
    PersistWatermark {
        /// The highest event id folded into the store.
        event_id: u64,
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
}

/// What a challenge signature is for. Named so a host cannot sign one thing and
/// present it as another.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChallengePurpose {
    /// Proving possession of the device key to the coordinator.
    Pairing,
    /// Refreshing an expiring credential.
    Refresh,
}

/// One typed frame for the Sync socket.
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
        /// The socket the records were sent on.
        socket_id: String,
    },
    /// Subscribe to one domain, exactly.
    Subscribe {
        /// The domain.
        domain: SyncDomain,
    },
    /// Unsubscribe from one domain, exactly.
    Unsubscribe {
        /// The domain.
        domain: SyncDomain,
    },
    /// Close one domain's snapshot/live gap, presenting the snapshot token.
    DomainReady {
        /// The domain.
        domain: SyncDomain,
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
        /// The generation the command belongs to.
        token: TerminalToken,
    },
    /// Request a fresh complete baseline.
    TerminalResync {
        /// The session.
        session_id: String,
        /// The view whose geometry the baseline must match.
        view_id: String,
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
}

/// One typed command for a direct carrier.
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
    },
    /// Request a fresh complete baseline over the direct link.
    Resync {
        /// The session.
        session_id: String,
        /// The view whose geometry the baseline must match.
        view_id: String,
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
    },
}

/// One Connect unary call.
///
/// A closed set on purpose: an open-ended request enum is how a client crate
/// grows a transport. Adding a member is a deliberate edit, and adding one is
/// also the moment to decide whether the state machine needs a new event for the
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RpcCall {
    /// `AuthCoordIdentity` — who this credential is.
    CoordIdentity {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `SessionsList` — the bootstrap snapshot, and the source of the one-time
    /// terminal snapshot token.
    SessionsList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// `WorkersList` — the worker registry, for the sidebar.
    WorkersList {
        /// Correlates the answer with this call.
        call_id: u64,
    },
    /// Redeem a pairing token.
    RedeemPairToken {
        /// Correlates the answer with this call.
        call_id: u64,
        /// The token the operator pasted.
        token: String,
    },
}

/// One Connect unary response.
/// Only `PartialEq`: the rows it carries are wire types, which are `PartialEq`
/// and not `Eq`.
#[derive(Debug, Clone, PartialEq)]
pub enum RpcResult {
    /// The call failed. The client reports it; it does not retry on its own,
    /// because every call here is either idempotent (in which case the host's
    /// dial loop decides) or a ceremony step a human drives.
    Failed {
        /// Which call this answers.
        call_id: u64,
        /// The coordinator's status text.
        message: String,
    },
    /// `AuthCoordIdentity` succeeded.
    CoordIdentity {
        /// Which call this answers.
        call_id: u64,
        /// The account this credential belongs to.
        account_id: String,
    },
    /// `SessionsList` succeeded.
    SessionsList {
        /// Which call this answers.
        call_id: u64,
        /// The complete session rows. The shared `SessionMap`, not a re-keyed
        /// map: converting branded ids back into strings and re-parsing them
        /// would be a second parse of the same rows, and a row whose id failed
        /// the brand check would be dropped silently.
        sessions: roost_protocol::wire::SessionMap,
        /// The one-time terminal hydration token, absent when the account has no
        /// terminal sessions.
        terminal_snapshot_token: Option<String>,
    },
    /// `WorkersList` succeeded.
    WorkersList {
        /// Which call this answers.
        call_id: u64,
        /// The worker rows, keyed by fingerprint.
        workers: std::collections::BTreeMap<String, roost_protocol::wire::Worker>,
    },
    /// A pairing token was redeemed.
    PairTokenRedeemed {
        /// Which call this answers.
        call_id: u64,
    },
}
