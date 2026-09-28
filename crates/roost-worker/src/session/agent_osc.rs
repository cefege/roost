//! The OSC title and progress a session's byte stream has carried. Read by
//! `crate::agents` when no integration reports a status, and cleared by it when
//! one agent is replaced by another. Depends on nothing, and on nothing that
//! depends on it back.

/// The longest tail of a split OSC 0/2 or OSC 9 sequence kept for the next
/// chunk. A chatty title change is small; 1 KiB is far above any of them, and
/// the bound is on the carry rather than on the title because the title is
/// replaced wholesale while the carry is only ever prepended to.
pub const AGENT_OSC_CARRY_MAX: usize = 1024;

/// What a session's OSC title and progress sequences have said so far.
///
/// Agent-state detection falls back to reading these directly off the PTY
/// stream when no agent extension reports in, so the evidence has to outlive
/// the chunk that carried it: `carry` holds the tail of a sequence split across
/// a chunk boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentOscState {
    /// The tail of an unterminated OSC sequence, at most
    /// [`AGENT_OSC_CARRY_MAX`] bytes.
    pub carry: Vec<u8>,
    /// The most recent complete OSC 0/2 title.
    pub raw_title: String,
    /// The most recent complete OSC 9 progress value.
    pub raw_progress: String,
}

impl AgentOscState {
    /// Forget the retained title and progress, keeping the carry.
    ///
    /// Called when a session's identified agent is REPLACED: a new process
    /// must not be judged by the dead one's final title, and the first
    /// acquisition keeps the bytes the new process emitted before the scan
    /// recognised it.
    pub fn clear_evidence(&mut self) {
        self.raw_title.clear();
        self.raw_progress.clear();
    }
}
