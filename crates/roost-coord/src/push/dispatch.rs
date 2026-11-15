//! Shared target selection and session metadata for Web Push notifications.
//! Session-scoped delivery suppresses devices already viewing the terminal.
//! Pairing and terminal notification dispatch use the same endpoint policy.

use std::collections::BTreeSet;

use roost_protocol::wire::SessionId;
use sqlx::{AnyPool, Row};

use crate::push::endpoint_policy::endpoint_origin;
use crate::push::subscription_store::StoredSubscription;

/// The kind carried by a session notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PushTransition {
    /// A terminal command or notification completed.
    Done,
}

/// Which browser devices are watching one session right now.
///
/// A device in this set is not told: it is already looking at the terminal, so
/// a push would be a second copy of what is on the screen.
pub trait ActiveTerminalViewers: Send + Sync {
    /// The device fingerprints currently viewing `session_id`.
    fn active_viewer_fingerprints(&self, session_id: &SessionId) -> BTreeSet<String>;
}

/// Split the deliverable set into the ones to notify and the count suppressed.
///
/// A row whose endpoint no longer parses, or whose origin is no longer on the
/// allowlist, is DROPPED rather than refused: the operator changed
/// `ROOST_PUSH_ALLOWED_ORIGINS` after these rows were written, and a
/// subscription that predates the change must not keep receiving. Re-validating
/// at send time is the whole reason this filter exists
/// (`push-dispatch.ts:92-108`).
pub(super) fn select_targets(
    subscriptions: &[StoredSubscription],
    allowed_origins: &[String],
    viewing: &BTreeSet<String>,
) -> (Vec<StoredSubscription>, usize) {
    let mut targets = Vec::with_capacity(subscriptions.len());
    let mut suppressed = 0;
    for subscription in subscriptions {
        if viewing.contains(&subscription.viewer_fp) {
            suppressed += 1;
            continue;
        }
        let Some(origin) = endpoint_origin(&subscription.endpoint) else {
            continue;
        };
        if allowed_origins.contains(&origin) {
            targets.push(subscription.clone());
        }
    }
    (targets, suppressed)
}

/// The open session's `cwd` and `custom_title`, or `None` when it is not open.
///
/// The `status = 'open'` predicate is load-bearing: a session closed between the
/// transition and this query must not produce a notification about work that no
/// longer exists.
pub(super) async fn open_session(
    pool: &AnyPool,
    session_id: &str,
) -> Option<(String, Option<String>)> {
    let row = match sqlx::query(
        "SELECT cwd, custom_title FROM sessions WHERE id = $1 AND status = 'open'",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await
    {
        Ok(row) => row?,
        Err(error) => {
            tracing::warn!(%session_id, %error, "push.open_session_lookup_failed");
            return None;
        }
    };
    Some((
        row.try_get::<String, _>("cwd").unwrap_or_default(),
        row.try_get::<Option<String>, _>("custom_title")
            .ok()
            .flatten(),
    ))
}

/// The notification title: the operator's title, else the directory's leaf.
///
/// The leaf is the last non-empty path segment split on BOTH separators, so a
/// Windows-authored path does not show as one long segment
/// (`push-dispatch.ts:118-119`). An empty cwd falls through to the product name
/// rather than to an empty notification.
pub(super) fn session_title(cwd: &str, custom_title: Option<&str>) -> String {
    if let Some(title) = custom_title.filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    cwd.split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}
