//! The shape of a transfer card and the state it can be in.
//!
//! Split from the state machine because the state names and the LEGAL EDGES are
//! one table, and the edges are what a caller gets wrong: `may_follow` is a total
//! function so "which transitions are legal" has one answer in the crate, not a
//! chain of `if`s that each call site has to keep in step.

use std::collections::BTreeMap;

/// Which way the bytes are going.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransferDirection {
    /// Browser to worker.
    Up,
    /// Worker to browser.
    Down,
}

impl TransferDirection {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Up => "up",
            Self::Down => "down",
        }
    }
}

/// Where a transfer is in its life.
///
/// `Stalled` is the one state v2 has no name for: a card that stopped advancing.
/// It is a deadline, not a verdict — the sweep moves a `running` card here when
/// nothing has advanced it for the parent's `TRANSFER_STALL_AFTER_MS`, and the
/// next progress tick moves it back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TransferState {
    /// The card exists and nothing has been sent yet.
    Queued,
    /// The bytes are being hashed for a content-dedup probe.
    Hashing,
    /// Bytes are moving.
    Running,
    /// Bytes stopped moving. A progress tick revives the card.
    Stalled,
    /// Finished. Dismissed on its own shortly after.
    Done,
    /// The worker already held these exact bytes, so nothing was sent.
    Dedup,
    /// Failed, with the reason. Stays until the user closes it.
    Failed,
    /// The bytes left the browser and nothing settled them: the write MAY have
    /// been committed, and the client cannot tell.
    ///
    /// The one state v2 has no name for, and the one a card must never paper
    /// over with `Failed`. A rejected write left nothing behind, so re-sending
    /// it is free; an ambiguous write may already be on the worker, so
    /// re-sending it is a doubled upload. Only the user can resolve which one
    /// this was, which is why the state is terminal AND does not self-dismiss.
    Ambiguous,
}

impl TransferState {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Hashing => "hashing",
            Self::Running => "active",
            Self::Stalled => "stalled",
            Self::Done => "ok",
            Self::Dedup => "dedup",
            Self::Failed => "err",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// Whether the card is settled: neither a progress tick nor a later
    /// transition applies to it. Only a fresh card for the same id replaces it.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Dedup | Self::Failed | Self::Ambiguous
        )
    }

    /// Whether `next` may follow this state.
    ///
    /// A total function, so "which transitions are legal" has one answer in the
    /// crate rather than a chain of `if`s at each call site. A refused edge is a
    /// caller bug and is reported as one; accepting it silently is how a card
    /// ends up `ok` with a live byte count.
    pub const fn may_follow(self, next: Self) -> bool {
        match self {
            Self::Queued => matches!(
                next,
                Self::Hashing
                    | Self::Running
                    | Self::Stalled
                    | Self::Done
                    | Self::Dedup
                    | Self::Failed
                    | Self::Ambiguous
            ),
            Self::Hashing => matches!(
                next,
                Self::Running | Self::Dedup | Self::Done | Self::Failed | Self::Ambiguous
            ),
            Self::Running => matches!(
                next,
                Self::Stalled | Self::Done | Self::Dedup | Self::Failed | Self::Ambiguous
            ),
            Self::Stalled => matches!(
                next,
                Self::Running | Self::Done | Self::Failed | Self::Ambiguous
            ),
            Self::Done | Self::Dedup | Self::Failed | Self::Ambiguous => false,
        }
    }

    /// Whether a card in this state removes ITSELF once its window runs out.
    ///
    /// Separate from [`TransferState::is_terminal`], because "nothing more can
    /// change this card" and "this card goes away by itself" are different
    /// claims. v2 draws the line the same way (`transfers.ts:89-98`): a
    /// successful or deduplicated card dismisses itself, and an ERROR card
    /// stays until the user closes it, because the failure text is the thing the
    /// user needs to read. An AMBIGUOUS card stays for the same reason and one
    /// more: the decision it asks for is the user's, and a card that removed
    /// itself would take that decision away.
    pub const fn self_dismisses(self) -> bool {
        matches!(self, Self::Done | Self::Dedup)
    }
}

/// The bound on the uncollected removals. A host that never drains loses the
/// OLDEST card's release obligation rather than growing without limit.
const MAX_RETAINED_REMOVED: usize = 64;

/// Everything one card needs in order to exist.
///
/// A struct rather than eight positional parameters, because six of the eight
/// are silently transposable: swapping `direction` for `state`, or
/// `bytes_total` for `now_ms`, compiles and produces a card that is wrong in a
/// way no assertion in the suite was written to catch. A name at the call site
/// is the only thing that makes the swap visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTransfer {
    pub id: String,
    pub name: String,
    pub direction: TransferDirection,
    pub bytes_total: u64,
    pub state: TransferState,
    pub preview_url: Option<String>,
    pub now_ms: u64,
}

/// One card.
#[derive(Debug, Clone, PartialEq)]
pub struct Transfer {
    /// The card's identity, chosen by the host: the attachment or download id.
    pub id: String,
    /// The file name, for the card's title.
    pub name: String,
    /// Which way the bytes are going.
    pub direction: TransferDirection,
    /// How many bytes have been accounted for. It may DECREASE: a chunked
    /// upload that restarts re-sends from the beginning.
    pub bytes_done: u64,
    /// The total, or zero when a download has not been told one yet.
    pub bytes_total: u64,
    /// Bytes per second, smoothed. `None` until the first real rate exists.
    pub speed_bps: Option<f64>,
    /// Seconds remaining, or `None` when the speed or the total is unknown.
    pub eta_s: Option<u64>,
    /// Where it is in its life.
    pub state: TransferState,
    /// The failure text, when it failed.
    pub err: Option<String>,
    /// The preview the host minted for this card. The HOST owns the underlying
    /// object URL and revokes it when the card leaves the map — see
    /// [`TransferStack::take_removed`].
    pub preview_url: Option<String>,
    /// When a settled card removes itself, or `None` while it is unsettled.
    pub dismiss_at_ms: Option<u64>,
    /// When the last progress tick landed, for the stall deadline.
    pub last_progress_ms: Option<u64>,
}

impl Transfer {
    /// The fraction complete in `0.0..=1.0`, or `None` when the total is
    /// unknown. A restart makes this fall, which is what it should do.
    pub fn fraction(&self) -> Option<f64> {
        if self.bytes_total == 0 {
            return None;
        }
        Some((self.bytes_done as f64 / self.bytes_total as f64).clamp(0.0, 1.0))
    }
}

/// One rate sample. `Copy`, because it is copied and never retained.
///
/// `pub(super)`, and not public: the state machine in the parent owns the rate
/// arithmetic and nothing else has any business seeding a sample.
///
/// Deliberately NOT `PartialEq`. `speed_bps` is an exponential moving average,
/// so an equality over samples is a comparison nothing should be able to make
/// — and making the type comparable is how that comparison gets made later by
/// someone who does not know what an EMA is.
#[derive(Debug, Clone, Copy)]
pub(super) struct RateSample {
    pub(super) at_ms: u64,
    pub(super) bytes: u64,
    pub(super) speed_bps: f64,
}

/// The live cards, and the bookkeeping beside them.
///
/// Deliberately NOT `PartialEq`, and the reason is the two fields that are not
/// cards. The samples are arithmetic a host never renders, and the uncollected
/// removals are an obligation the host has not discharged yet, so two stacks
/// differing only in those are the same stack to every reader that exists.
/// There is also no comparison to make: nothing compares two stacks, and a
/// `PartialEq` nothing calls is a promise the code does not keep. Compare the
/// cards through [`TransferStack::transfers`] when a caller appears.

#[derive(Debug, Default, Clone)]
pub struct TransferStack {
    pub(super) transfers: BTreeMap<String, Transfer>,
    pub(super) samples: BTreeMap<String, RateSample>,
    removed: Vec<Transfer>,
}

impl TransferStack {
    /// No cards.
    pub fn new() -> Self {
        Self::default()
    }

    /// The live cards, by id.
    pub fn transfers(&self) -> impl Iterator<Item = &Transfer> {
        self.transfers.values()
    }

    /// One card.
    pub fn transfer(&self, id: &str) -> Option<&Transfer> {
        self.transfers.get(id)
    }

    /// How many cards are live.
    pub fn len(&self) -> usize {
        self.transfers.len()
    }

    /// Whether no card is live.
    pub fn is_empty(&self) -> bool {
        self.transfers.is_empty()
    }

    /// Take the cards that have left the map, for the host to release whatever
    /// it minted for them.
    ///
    /// This is bookkeeping rather than rendered state: it changes without a
    /// `revision`, and it is why removal needs no new `Effect` variant for a
    /// host to learn. A host drains it after each `handle`.
    pub fn take_removed(&mut self) -> Vec<Transfer> {
        std::mem::take(&mut self.removed)
    }

    pub(super) fn drop_card(&mut self, id: &str) {
        if let Some(transfer) = self.transfers.remove(id) {
            self.samples.remove(id);
            if self.removed.len() < MAX_RETAINED_REMOVED {
                self.removed.push(transfer);
            }
        }
    }
}
