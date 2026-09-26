//! Terminal input: the vocabulary of an admitted batch, the caps it is held to,
//! and the router in `router` that owns them.
//!
//! The three-valued outcome is the whole point. `accepted` means the PTY took
//! the bytes. `rejected` means it did not, and the batch is never retried.
//! `ambiguous` means the client cannot tell — and an ambiguous batch is ALSO
//! never retried, because a retried ambiguous batch is a doubled keystroke in
//! someone's shell (`protocol/spec/direct-terminal.md:29`).
//!
//! Held input is the other half. While a route is reconnecting or claiming, a
//! batch is admitted and held UNSENT, with an admission timeout. It is released
//! when a destination appears; if the timeout fires first it settles `rejected`,
//! which is safe precisely because nothing was sent.
//!
//! Ported from `apps/web/src/store/transport/terminal-input-router.ts` and
//! `apps/web/src/client/carriers/terminal-input-lanes.ts`. The reasons are in
//! `docs/phase4-client-contract.md` §9; the incident is
//! `docs/FAILURE-INDEX.md:1503`, "Delayed old-route input crosses a
//! direct-promotion fence".

pub mod router;

pub use router::InputRouter;

use crate::terminal::token::TerminalToken;

/// Largest single batch. A batch bigger than a peer frame ceiling can never be
/// written, so the lane would hold bytes that can only ever be refused.
pub const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Most batches one session may have outstanding. A stuck route turns a held
/// lane into an unbounded queue, and that queue is what the user is typing into.
pub const MAX_PENDING_INPUTS_PER_SESSION: usize = 200;

/// Most bytes one session may have outstanding, for the same reason with
/// paste-sized input: a 200-batch cap does not bound a paste.
pub const MAX_PENDING_INPUT_BYTES_PER_SESSION: usize = 256 * 1024;

/// How long a held batch waits for a route before it is refused.
///
/// It settles `rejected`, not `ambiguous`, and that is the safety of the whole
/// hold: nothing was sent, so refusing it cannot lose a keystroke.
pub const HELD_INPUT_ADMISSION_TIMEOUT_MS: u64 = 10_000;

/// The highest route revision the wire's signed 64-bit field can carry. The
/// client refuses to issue a claim past it rather than wrapping a revision the
/// worker would read as an older one
/// (`apps/web/src/store/transport/terminal-input-route-claim.ts:14`).
pub const MAX_TERMINAL_INPUT_ROUTE_REVISION: u64 = i64::MAX as u64;

/// What became of one admitted batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputOutcome {
    /// Written to the PTY.
    Accepted {
        /// The batch's own sequence, which the result is correlated by.
        input_seq: u64,
        /// How many bytes the keeper reported writing. Fewer than were sent
        /// means the keeper refused part of the batch, which the pane shows.
        written_bytes: u32,
    },
    /// Not written. Never retried.
    Rejected {
        /// The batch's own sequence.
        input_seq: u64,
        /// Why, for the pane and the log.
        reason: String,
    },
    /// The client cannot tell whether it was written. Never retried, never
    /// replayed, never drained.
    Ambiguous {
        /// The batch's own sequence.
        input_seq: u64,
        /// How many bytes were handed to the transport, which is the most that
        /// could have been written.
        written_bytes: u32,
        /// Why the client cannot tell.
        reason: String,
    },
}

impl InputOutcome {
    /// The batch this outcome answers.
    pub const fn input_seq(&self) -> u64 {
        match self {
            Self::Accepted { input_seq, .. }
            | Self::Rejected { input_seq, .. }
            | Self::Ambiguous { input_seq, .. } => *input_seq,
        }
    }

    /// Whether this outcome permits a retry.
    ///
    /// Never, for any variant. The method exists so the answer is one named
    /// thing rather than each reader's judgement — a reader who has to decide
    /// this at a call site will eventually decide it differently somewhere.
    pub const fn is_retryable(&self) -> bool {
        false
    }

    /// Whether this outcome leaves the client in doubt about the PTY.
    pub const fn is_ambiguous(&self) -> bool {
        matches!(self, Self::Ambiguous { .. })
    }

    /// The wire spelling of the outcome, for logs and for the pane's status line.
    pub const fn status_name(&self) -> &'static str {
        match self {
            Self::Accepted { .. } => "accepted",
            Self::Rejected { .. } => "rejected",
            Self::Ambiguous { .. } => "ambiguous",
        }
    }
}

/// Why a batch was refused at admission, before it was ever queued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRefusal {
    /// The reason, which is what the pane shows the user.
    pub reason: String,
}

/// The carrier identity a batch was dispatched on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFence {
    /// The exact generation, so a retirement on a new socket cannot be confused
    /// with a retirement of the old one.
    pub token: TerminalToken,
}

impl TerminalFence {
    /// A fence for one generation.
    pub fn new(token: TerminalToken) -> Self {
        Self { token }
    }
}

/// One admitted batch's bookkeeping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInput {
    /// The session.
    pub session_id: String,
    /// The view the keystroke came from, when it came from one.
    pub view_id: Option<String>,
    /// The client-allocated sequence, allocated at ADMISSION so a result can be
    /// correlated even when the batch is refused before it is ever sent.
    pub input_seq: u64,
    /// The bytes. Owned, because the front end's buffer does not outlive a hold.
    pub bytes: Vec<u8>,
    /// The carrier generation the batch was dispatched on, or `None` while it is
    /// still held. The lane never interprets the token; it carries it back so
    /// the retirement path can tell a started batch from an unstarted one.
    pub fence: Option<TerminalFence>,
    /// The bytes have been handed to a transport. Past this point the outcome
    /// can be `ambiguous`, which is why nothing about a started batch is ever
    /// replayed.
    pub started: bool,
    /// When the batch was admitted, for the hold timeout.
    pub admitted_at_ms: u64,
}

/// Where a session's input is in its route lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputPhase {
    /// Sending normally.
    Sending,
    /// The route is reconnecting. Batches are held unsent.
    Holding,
    /// The worker is being asked for a route epoch. Batches are held unsent.
    Claiming,
    /// The route is known broken. Batches are refused.
    Blocked,
    /// The session is closed. Batches are refused.
    Closed,
}

/// One session's input lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputLane {
    /// The session.
    pub session_id: String,
    /// Where the route is.
    pub phase: InputPhase,
    /// Outstanding batches, oldest first.
    pub pending: Vec<PendingInput>,
    /// The bytes outstanding, for the byte cap.
    pub pending_bytes: usize,
    /// The route epoch the worker acknowledged, or empty when the worker does
    /// not implement `terminal-input-route-v1`.
    pub route_epoch: String,
    /// The revision the last claim used.
    pub route_revision: u64,
    /// The generation the acknowledged epoch belongs to. A new generation has
    /// never been acknowledged, so its epoch is empty even though the previous
    /// generation's was not.
    pub route_epoch_token: Option<TerminalToken>,
    /// Batches whose fate the client cannot report, so a drain cannot complete.
    pub ambiguous: Vec<u64>,
}
