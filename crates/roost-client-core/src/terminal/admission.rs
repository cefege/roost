//! What admitting one frame did, and what applying one view-state result did.
//!
//! Split out of `session` because an outcome is a VERDICT and the replica that
//! reaches it is state: this file is what you read when a caller has to know
//! whether to repaint, and `session` is what you read when it already knows.
//!
//! Contract: `protocol/spec/terminal-stream.md:22-30`.

use crate::terminal::token::TerminalToken;
/// What admitting one frame did, for the host that renders it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admission {
    /// Nothing changed. `latched` says whether this call is the one that latched
    /// the repair; the replica is untouched either way.
    Refused {
        /// The contract reason, or the diagnosis when there is no contract code.
        reason: String,
        /// True when this call is the one that latched.
        latched: bool,
    },
    /// A complete baseline replaced the replica. The host may repaint from
    /// `canonical()`; renderers keep their last complete DOM until then.
    BaselineReplaced,
    /// A delta extended the replica. The host may apply the row changes to the
    /// rows it already painted.
    DeltaApplied,
    /// A chunk is still assembling; nothing to paint yet.
    ChunkPending,
}

impl Admission {
    /// A frame this replica is not the destination of.
    ///
    /// It never latches: the replica is not damaged, it is simply not the
    /// frame's destination — a frame from a retired generation is routine. It
    /// still NAMES ITSELF, because a silent fence is also exactly what a lost
    /// baseline looks like, and that is not routine: the pane stays blank with
    /// a healthy lane at both ends and nothing anywhere says why.
    ///
    /// `bound` is what this replica holds, `frame_stream` is what the frame
    /// carried; naming both is what turns "it went blank" into a diagnosis.
    #[must_use]
    pub fn unbound(
        session_id: &str,
        fence: &'static str,
        bound: &str,
        frame_stream: &str,
        token: &TerminalToken,
    ) -> Self {
        tracing::debug!(
            target: "terminal",
            session_id,
            fence,
            frame_stream,
            bound,
            frame_socket_generation = token.socket_generation,
            frame_process_epoch = %token.process_epoch,
            frame_domain_generation = token.domain_generation,
            "a terminal frame was not this replica's; it is dropped unlatched"
        );
        Self::Refused {
            reason: format!("{fence} {bound} is not current for frame stream {frame_stream}"),
            latched: false,
        }
    }
}

/// What a view-state result did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewStateAdmission {
    /// The result belongs to a view that is not awaiting this generation. It is
    /// the answer to a command from a socket that has been replaced, and it
    /// changes nothing.
    Stale,
    /// The authority does not hold the view.
    Refused,
    /// The authority holds the view, and is minting this stream.
    Accepted {
        /// The stream the authority is now on, when it named one.
        stream_id: Option<String>,
    },
}
