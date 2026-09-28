//! The keeper WRITE-ORDERING lane: what a terminal write must do to land in the
//! order it was made, and the fail-closed gate that refuses one outright while a
//! keeper replacement is prepared. `super::control_lanes::ControlLanes::admit`
//! issues the tickets; `session::lifecycle` spends them around a resize and
//! `session::emit` around a query reply. Depends on `super::control_lanes` for
//! the gate — and on nothing that depends on it back.
//!
//! WHY A TICKET AND NOT A QUEUE. Two callers hold a write for different lengths
//! of time and need different things from the lane. A stream transaction wants
//! the lane until its resize request is ON THE WIRE and no longer, because input
//! behind a resize acknowledgement is a keystroke that arrives a frame late. A
//! query reply wants it for the length of one write and nothing else. A ticket
//! that is taken explicitly and given back at a stated boundary expresses both;
//! a lock held to the end of a scope expresses neither.
//!
//! RELEASE IS IDEMPOTENT, and that is the point. The transaction releases at its
//! ordering boundary and again on its way out, and the second call must not
//! advance the lane past a writer that has not been admitted — a double release
//! that handed the permit on twice would let two keepers writes overlap for
//! exactly as long as the first one takes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use roost_protocol::wire::brand::ChannelId;

use super::control_lanes::{Lane, lock, mono_ms};

/// The refusal every terminal write carries while a keeper replacement is
/// prepared. The coordinator maps it onto a pre-write rejection, so a client may
/// retry against the replacement keeper without duplicating the write.
pub const KEEPER_UPDATE_WRITE_REFUSAL: &str = "keeper update preparation blocks terminal writes";

/// One keeper write, which may not overtake another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AdmissionKind {
    TerminalResize,
    TerminalInput,
    QueryReply,
}

impl AdmissionKind {
    /// Whether this write is refused while a keeper replacement is prepared.
    ///
    /// A query reply is not: it is the core answering a probe the application is
    /// already blocked on, and withholding it strands a full-screen TUI on its
    /// handshake rather than on anything the update is protecting.
    pub fn blocked_by_keeper_update(self) -> bool {
        matches!(self, Self::TerminalResize | Self::TerminalInput)
    }

    /// The name a log line and a diagnostic report both spell it as.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TerminalResize => "terminal_resize",
            Self::TerminalInput => "terminal_input",
            Self::QueryReply => "query_reply",
        }
    }

    pub(super) const fn report_index(self) -> u32 {
        match self {
            Self::TerminalResize => 1,
            Self::TerminalInput => 2,
            Self::QueryReply => 3,
        }
    }

    /// The name a report's numeric field means, or nothing when the lane is idle.
    pub fn from_report_index(index: u32) -> Option<Self> {
        match index {
            1 => Some(Self::TerminalResize),
            2 => Some(Self::TerminalInput),
            3 => Some(Self::QueryReply),
            _ => None,
        }
    }
}

/// The write-ordering lane: a ticket, or the refusal that ended this attempt.
#[derive(Debug)]
pub enum Admission {
    Granted(AdmissionTicket),
    Refused(&'static str),
}

impl Admission {
    /// Whether a ticket was issued.
    pub fn is_granted(&self) -> bool {
        matches!(self, Self::Granted(_))
    }

    /// The refusal text, or nothing when a ticket was issued.
    pub fn refusal(&self) -> Option<&'static str> {
        match self {
            Self::Granted(_) => None,
            Self::Refused(reason) => Some(reason),
        }
    }
}

/// A held slot on the write-ordering lane.
///
/// The permit is taken by [`AdmissionTicket::granted`] and given back by
/// [`AdmissionTicket::release`], which is idempotent.
#[derive(Debug)]
pub struct AdmissionTicket {
    channel_id: ChannelId,
    kind: AdmissionKind,
    lane: Arc<Lane>,
    /// Never held across an await, so the ticket stays `Send` without putting a
    /// `tokio::sync::Mutex` in front of every keeper write.
    held: Mutex<Option<tokio::sync::OwnedSemaphorePermit>>,
    released: AtomicBool,
}

impl AdmissionTicket {
    pub(super) fn new(channel_id: ChannelId, kind: AdmissionKind, lane: Arc<Lane>) -> Self {
        Self {
            channel_id,
            kind,
            lane,
            held: Mutex::new(None),
            released: AtomicBool::new(false),
        }
    }

    /// Which write this slot is for.
    pub fn kind(&self) -> AdmissionKind {
        self.kind
    }

    /// Wait until every earlier keeper write has been released.
    ///
    /// A ticket that was already released takes nothing: the caller released at
    /// its boundary before ever entering the lane, and re-entering would queue it
    /// behind the very writes it was meant to precede.
    pub async fn granted(&self) {
        if self.released.load(Ordering::Acquire) {
            return;
        }
        match self.lane.gate.clone().acquire_owned().await {
            Ok(permit) => {
                self.lane
                    .running
                    .store(self.kind.report_index(), Ordering::Relaxed);
                self.lane.running_since.store(mono_ms(), Ordering::Relaxed);
                tracing::debug!(
                    channel_id = ?self.channel_id,
                    kind = self.kind.as_str(),
                    "a keeper write holds the write-ordering lane"
                );
                *lock(&self.held) = Some(permit);
            }
            Err(_) => {
                tracing::error!(
                    channel_id = ?self.channel_id,
                    kind = self.kind.as_str(),
                    "the write-ordering lane closed before this write entered it"
                );
            }
        }
    }

    /// Give the slot back.
    ///
    /// The boundary a caller MUST call this at is the moment its write is on the
    /// wire — not its acknowledgement, and not the snapshot that follows.
    pub fn release(&self) {
        if self.released.swap(true, Ordering::AcqRel) {
            return;
        }
        self.lane.running.store(0, Ordering::Relaxed);
        // Dropping the permit is what frees the next writer; the count goes down
        // after it, so a lane is only ever considered idle once the writer ahead
        // of it has genuinely left.
        drop(lock(&self.held).take());
        self.lane.waiting.fetch_sub(1, Ordering::AcqRel);
        tracing::debug!(
            channel_id = ?self.channel_id,
            kind = self.kind.as_str(),
            "a keeper write released the write-ordering lane"
        );
    }
}

impl Drop for AdmissionTicket {
    fn drop(&mut self) {
        self.release();
    }
}
