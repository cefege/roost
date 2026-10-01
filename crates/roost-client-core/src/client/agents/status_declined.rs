//! Why a status report reached this browser and still did nothing.
//!
//! Split from `status_projection` for the size cap. Every refusal there is a
//! `None` the caller cannot tell from "nothing changed", and that silence is
//! not cosmetic: it is how a reader's sidebar, tab chip and title badge keep
//! showing `working` for minutes after the agent reported `blocked` and the
//! coordinator accepted it.
//!
//! A refused report is a DECISION, and a decision that leaves the operator
//! looking at a stale state is the one that must be legible from a log.

use super::AgentStatusChange;

/// A report this profile declines, said out loud.
#[must_use]
pub(super) fn declined(session_id: &str, why: &str) -> Option<AgentStatusChange> {
    tracing::warn!(
        target: "sync",
        session_id,
        "an agent status report was declined by this profile: {why}"
    );
    None
}
