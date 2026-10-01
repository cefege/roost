//! The document title's attention count and the prefix rule that keeps it a
//! prefix. Ports `countUnseenAgentStatuses` and the title effect of
//! `apps/web/src/components/notifications/AgentNotificationBridge.tsx`.
//!
//! The count is DERIVED from the rows and the acknowledgement ledger on every
//! read. A second stored counter would be a fourth answer to "what does this
//! profile still owe a look at" — the ledger, the row level and the card already
//! are three — and the first one to disagree would be the title.

use roost_client_core::client::agents::AgentSeenLedger;
use roost_client_core::client::agents::status_policy::agent_status_completion_unseen;
use roost_protocol::wire::{AgentRuntimeState, AgentStatus};

/// The title the badge is drawn onto: the current one with a leading `(N)`
/// removed, or `Roost` when what is left is blank.
#[must_use]
pub fn base_title(current: &str) -> String {
    let bare = strip_count_prefix(current.trim());
    if bare.is_empty() {
        "Roost".to_owned()
    } else {
        bare.to_owned()
    }
}

/// The document title for `count` rows this profile has not acknowledged.
///
/// The count is rendered from `base`, never from the title already on the
/// document, so a re-application replaces the prefix instead of stacking a
/// second one on top of it.
#[must_use]
pub fn badge_title(base: &str, count: usize, enabled: bool) -> String {
    if enabled && count > 0 {
        format!("({count}) {base}")
    } else {
        base.to_owned()
    }
}

/// How many rows want this profile's attention.
#[must_use]
pub fn attention_count<'status>(
    statuses: impl Iterator<Item = &'status AgentStatus>,
    acknowledged: &AgentSeenLedger,
) -> usize {
    statuses
        .filter(|status| wants_attention(status, acknowledged.acknowledged_revision(status)))
        .count()
}

/// A blocked agent wants attention only while its own revision is
/// unacknowledged, and a finished one only while its completion is. Both are the
/// SAME predicates the row levels are built from, so the title and the rows it
/// sits above cannot disagree about what this profile has been told.
///
/// The blocked arm is revision-gated rather than level-matched on purpose:
/// acknowledging a blocked row does not unblock the agent, so a level test would
/// keep counting a row the operator is looking at, and the badge would be a
/// number nothing the reader does can move.
fn wants_attention(status: &AgentStatus, acknowledged: i64) -> bool {
    if status.common.state == AgentRuntimeState::Blocked {
        return status.common.revision > acknowledged;
    }
    agent_status_completion_unseen(status, Some(acknowledged))
}

/// The text after a leading `(N) `, or `title` itself when it carries no such
/// prefix — including a `(` that never closes and a `(N)` with letters in it,
/// both of which are somebody's window name rather than our prefix.
fn strip_count_prefix(title: &str) -> &str {
    let Some(rest) = title.strip_prefix('(') else {
        return title;
    };
    let Some(close) = rest.find(')') else {
        return title;
    };
    let digits = &rest[..close];
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return title;
    }
    rest[close + 1..].trim_start()
}
