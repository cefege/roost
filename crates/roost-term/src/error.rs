//! Why a frame could not be produced.
//!
//! Every variant here is a condition an operator or a client can be told
//! about. An emulator that cannot describe its own history is not a degraded
//! mode — it is a refusal, and the frame that would have been wrong is not
//! sent at all.

/// The result of building a frame from a terminal core.
pub type TerminalCoreResult<T> = Result<T, TerminalCoreError>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TerminalCoreError {
    /// The core cannot report how many history lines its ring has evicted, so
    /// no absolute history index can be trusted.
    ///
    /// Roost addresses scrollback by a monotonic index whose origin is the
    /// eviction count. Without it, a retained line and a line evicted a
    /// minute ago are indistinguishable, and every index a client holds
    /// re-aliases at the saturation point — the failure `docs/FAILURE-INDEX.md`
    /// records as history mis-splices. Guessing is worse than refusing: a
    /// caller cannot tell a correct frame from a guessed one, but it can tell
    /// a wrong history index from a right one.
    #[error(
        "terminal core does not report a discarded history line count: the scrollback \
         origin cannot be authoritative and every absolute history index would re-alias"
    )]
    NoDiscardedLineCount,
}
