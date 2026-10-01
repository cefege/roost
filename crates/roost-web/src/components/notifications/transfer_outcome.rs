//! How a transfer card settled, and the rule that an ambiguous write is never
//! retried. The classification is a pure function of the card, so the panel, the
//! row and any diagnostic agree on one reading.
//!
//! The three settled outcomes are DISTINCT on purpose. A card that reported
//! "the worker refused this" and a card that reported "we never learned whether
//! the worker took it" are different facts, and only the second is dangerous: a
//! retry of an ambiguous write re-sends bytes the worker may already hold. So
//! `TransferState::Ambiguous` is a state of its own, the card says what the user
//! is being asked to decide, and nothing here re-sends.

use roost_client_core::store::transfers::{Transfer, TransferState};

/// How a card settled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferOutcome {
    /// The bytes were written and the worker acknowledged them.
    Accepted,
    /// The worker already held these exact bytes, so nothing was sent.
    Deduplicated,
    /// The write was refused. Nothing was committed, so re-sending is free.
    Rejected,
    /// The write may or may not have been committed: the acknowledgement was
    /// lost. NEVER retried — a retried ambiguous write is a doubled upload.
    Ambiguous,
}

impl TransferOutcome {
    /// The wire spelling, for the card's `data-outcome` and for logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Deduplicated => "dedup",
            Self::Rejected => "rejected",
            Self::Ambiguous => "ambiguous",
        }
    }

    /// The `StatusDot` spelling a settled card's dot reads as.
    ///
    /// Ambiguous is a warning, not an error: nothing is known to be wrong, and a
    /// red card here would read as a refusal the user may safely retry.
    pub const fn dot_status(self) -> &'static str {
        match self {
            Self::Accepted | Self::Deduplicated => "ok",
            Self::Rejected => "error",
            Self::Ambiguous => "warn",
        }
    }

    /// Whether a card in this outcome may be sent again automatically.
    ///
    /// Never, for any of them — the same answer the client's own
    /// `InputOutcome::is_retryable` gives, and a named constant rather than each
    /// reader's judgement.
    pub const fn permits_automatic_retry(self) -> bool {
        false
    }
}

/// How `transfer` settled, or `None` while it is still moving. A row with no
/// outcome renders its progress instead of a verdict.
pub fn transfer_outcome(transfer: &Transfer) -> Option<TransferOutcome> {
    match transfer.state {
        TransferState::Done => Some(TransferOutcome::Accepted),
        TransferState::Dedup => Some(TransferOutcome::Deduplicated),
        TransferState::Failed => Some(TransferOutcome::Rejected),
        TransferState::Ambiguous => Some(TransferOutcome::Ambiguous),
        _ => None,
    }
}
