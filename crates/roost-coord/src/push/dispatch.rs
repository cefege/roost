//! Per-device Web Push preparation for a delayed coding-agent transition.
//!
//! Owned by the push domain. `fire_push_for_transition` is the entry point the
//! agents domain calls when an agent blocks or finishes; it decides WHO gets
//! told, and `sender` decides whether they were.
//!
//! THE VIEWS ARE A PARAMETER, NOT A CALL. v2 imports
//! `activeTerminalViewerFingerprints` from
//! `terminal/view/terminal-view-hub.ts` (`push-dispatch.ts:9`). That module is a
//! later wave, and a crate-global here would make every coordinator test share
//! one set of viewers -- so the answer is a trait, the same discipline
//! `sync_ws/terminal/snapshot.rs` used for `TerminalSnapshotHub`.
//!
//! THE FENCE IS REVALIDATED ASYNCHRONOUSLY. `is_current` is checked before the
//! database work, after it, and again inside the sender before each individual
//! send (`push-dispatch.ts:47,132-141`). A transition that was superseded while
//! the query ran must not notify anybody, and the window is exactly as wide as
//! the query.

use std::collections::HashSet;
use std::sync::Arc;

use roost_observability::LogFields;
use roost_protocol::wire::{AgentOccupantId, SessionId, StatusEpoch};
use serde::Serialize;
use sha2::Digest;
use sqlx::{Row, SqlitePool};

use crate::push::endpoint_policy::endpoint_origin;
use crate::push::sender::{PushDeliveryOptions, PushDeliveryResult, send_push_to_subscriptions};
use crate::push::subscription_store::{StoredSubscription, take_deliverable_subscriptions};
use crate::push::transport::PushNotificationTransport;

/// What a transition did, as the notification body and the topic both need it.
///
/// The shared value `push-dispatch.ts:14` exports. It is a type here rather
/// than a string because every use of it here is exhaustive; a caller that
/// forgot an arm would not compile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PushTransition {
    /// The agent is waiting on a human.
    Blocked,
    /// The agent finished on its own.
    Done,
}

impl PushTransition {
    /// The word the payload carries.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    /// The sentence a phone shows.
    #[must_use]
    pub fn notification_body(self) -> &'static str {
        match self {
            Self::Blocked => "Needs your input",
            Self::Done => "Finished",
        }
    }
}

/// One agent status transition, with everything needed to decide whether its
/// notification is still the truth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPushTransition {
    /// The session the agent runs in.
    pub session_id: SessionId,
    /// Blocked or done.
    pub kind: PushTransition,
    /// The epoch the status was observed in. A stale epoch is a stale
    /// notification even when the kind matches.
    pub status_epoch: StatusEpoch,
    /// Which occupant produced it, so two agents in one session do not share a
    /// deduplication token.
    pub occupant_id: AgentOccupantId,
    /// Revision within the epoch, retained across same-state updates so a
    /// retried publish does not re-notify.
    pub revision: u64,
}

/// Which browser devices are watching one session right now.
///
/// A device in this set is not told: it is already looking at the terminal, so
/// a push would be a second copy of what is on the screen.
pub trait ActiveTerminalViewers: Send + Sync {
    /// The device fingerprints currently viewing `session_id`.
    fn active_viewer_fingerprints(&self, session_id: &str) -> HashSet<String>;
}

/// A coordinator with no terminal-view hub wired, so nobody is ever viewing.
///
/// The value a caller passes when the terminal domain has not landed. It is a
/// VALUE rather than an `Option` so a caller cannot forget the argument, and it
/// matches the `NoTerminalSnapshotHub` precedent.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoTerminalViewers;

impl ActiveTerminalViewers for NoTerminalViewers {
    fn active_viewer_fingerprints(&self, _session_id: &str) -> HashSet<String> {
        HashSet::new()
    }
}

/// What one notification says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentPushPayload {
    /// The session the agent runs in.
    #[serde(rename = "sessionId")]
    pub session_id: String,
    /// `blocked` or `done`.
    pub kind: PushTransition,
    /// The session's title, or its directory's leaf name.
    pub title: String,
    /// The sentence a phone shows.
    pub body: String,
    /// The status epoch this notification is about.
    #[serde(rename = "statusEpoch")]
    pub status_epoch: String,
    /// The occupant that produced it.
    #[serde(rename = "occupantId")]
    pub occupant_id: String,
    /// The revision within the epoch.
    pub revision: u64,
    /// The RFC 8030 topic, so a retried send replaces rather than stacks.
    #[serde(rename = "deduplicationToken")]
    pub deduplication_token: String,
}

/// Prepare and deliver the notification for one agent transition.
///
/// Never fails: a transition is not worth an error the caller has to handle, so
/// every refusal and every failure ends in a log line
/// (`push-dispatch.ts:160-169`). The agents domain calls this from a status
/// update and has nothing useful to do with an error.
#[allow(clippy::too_many_arguments)]
pub async fn fire_push_for_transition(
    pool: &SqlitePool,
    transition: &AgentPushTransition,
    allowed_origins: &[String],
    viewers: &dyn ActiveTerminalViewers,
    is_current: &Arc<dyn Fn() -> bool + Send + Sync>,
    transport: &dyn PushNotificationTransport,
) {
    let session_id = transition.session_id.as_str();
    let kind = transition.kind.as_str();
    if allowed_origins.is_empty() || !is_current() {
        return;
    }

    let Some(session) = open_session(pool, session_id).await else {
        roost_observability::log::info(
            "push",
            "session_missing",
            LogFields::new()
                .set("session_id", session_id)
                .set("kind", kind),
        );
        return;
    };
    let subscriptions = match take_deliverable_subscriptions(pool).await {
        Ok(subscriptions) => subscriptions,
        Err(error) => {
            log_dispatch_failed(transition, &error.to_string());
            return;
        }
    };
    if subscriptions.is_empty() {
        return;
    }

    let viewing = viewers.active_viewer_fingerprints(session_id);
    let (targets, suppressed) = select_targets(&subscriptions, allowed_origins, &viewing);
    if targets.is_empty() {
        roost_observability::log::info(
            "push",
            "suppressed_all",
            LogFields::new()
                .set("session_id", session_id)
                .set("kind", kind)
                .set("subscriptions", subscriptions.len()),
        );
        return;
    }

    let payload = AgentPushPayload {
        session_id: session_id.to_owned(),
        kind: transition.kind,
        title: session_title(session.0.as_str(), session.1.as_deref()),
        body: transition.kind.notification_body().to_owned(),
        status_epoch: transition.status_epoch.as_str().to_owned(),
        occupant_id: transition.occupant_id.as_str().to_owned(),
        revision: transition.revision,
        deduplication_token: deduplication_token(transition),
    };
    let deduplication_token = payload.deduplication_token.clone();
    let body = match serde_json::to_string(&payload) {
        Ok(body) => body,
        Err(error) => {
            log_dispatch_failed(transition, &error.to_string());
            return;
        }
    };

    if !is_current() {
        roost_observability::log::info(
            "push",
            "status_superseded",
            LogFields::new()
                .set("session_id", session_id)
                .set("kind", kind)
                .set("status_epoch", transition.status_epoch.as_str())
                .set("occupant_id", transition.occupant_id.as_str())
                .set("revision", transition.revision),
        );
        return;
    }

    let result = send_push_to_subscriptions(
        pool,
        &targets,
        &body,
        PushDeliveryOptions {
            deduplication_token: Some(deduplication_token.clone()),
            is_current: Some(Arc::clone(is_current)),
        },
        transport,
    )
    .await;
    log_dispatched(
        transition,
        subscriptions.len(),
        suppressed,
        targets.len(),
        result,
    );
}

/// Split the deliverable set into the ones to notify and the count suppressed.
///
/// A row whose endpoint no longer parses, or whose origin is no longer on the
/// allowlist, is DROPPED rather than refused: the operator changed
/// `ROOST_PUSH_ALLOWED_ORIGINS` after these rows were written, and a
/// subscription that predates the change must not keep receiving. Re-validating
/// at send time is the whole reason this filter exists
/// (`push-dispatch.ts:92-108`).
fn select_targets(
    subscriptions: &[StoredSubscription],
    allowed_origins: &[String],
    viewing: &HashSet<String>,
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
async fn open_session(pool: &SqlitePool, session_id: &str) -> Option<(String, Option<String>)> {
    sqlx::query("SELECT cwd, custom_title FROM sessions WHERE id = ?1 AND status = 'open'")
        .bind(session_id)
        .fetch_optional(pool)
        .await
        .ok()?
        .map(|row| {
            (
                row.try_get::<String, _>("cwd").unwrap_or_default(),
                row.try_get::<Option<String>, _>("custom_title")
                    .ok()
                    .flatten(),
            )
        })
}

/// The notification title: the operator's title, else the directory's leaf.
///
/// The leaf is the last non-empty path segment split on BOTH separators, so a
/// Windows-authored path does not show as one long segment
/// (`push-dispatch.ts:118-119`). An empty cwd falls through to the product name
/// rather than to an empty notification.
fn session_title(cwd: &str, custom_title: Option<&str>) -> String {
    if let Some(title) = custom_title.filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    cwd.split(['/', '\\'])
        .rfind(|segment| !segment.is_empty())
        .unwrap_or(cwd)
        .to_owned()
}

/// Thirty-two base64url characters derived from the transition's identity.
///
/// The five identity fields are hashed together, so two transitions of the same
/// session in the same epoch by the same occupant at different revisions get
/// DIFFERENT tokens -- the RFC 8030 topic then replaces a stale notification
/// rather than deduplicating a new one away.
fn deduplication_token(transition: &AgentPushTransition) -> String {
    // `serde_json::json!` and `JSON.stringify` agree on a heterogeneous array:
    // no spaces, strings double-quoted, the number bare. v2 hashes exactly
    // those bytes, and a token that differed from v2's would defeat the
    // RFC 8030 topic replacement for a coordinator that upgraded in place.
    let identity = serde_json::to_vec(&serde_json::json!([
        transition.session_id.as_str(),
        transition.kind.as_str(),
        transition.status_epoch.as_str(),
        transition.occupant_id.as_str(),
        transition.revision,
    ]))
    .unwrap_or_default();
    let digest = sha2::Sha256::digest(&identity);
    roost_host::b64url_encode(&digest)[..32].to_owned()
}

/// The one line that says what the dispatch did.
#[allow(clippy::too_many_arguments)]
fn log_dispatched(
    transition: &AgentPushTransition,
    subscriptions: usize,
    suppressed: usize,
    targeted: usize,
    result: PushDeliveryResult,
) {
    roost_observability::log::info(
        "push",
        "dispatched",
        LogFields::new()
            .set("session_id", transition.session_id.as_str())
            .set("kind", transition.kind.as_str())
            .set("status_epoch", transition.status_epoch.as_str())
            .set("occupant_id", transition.occupant_id.as_str())
            .set("revision", transition.revision)
            .set("subscriptions", subscriptions)
            .set("suppressed", suppressed)
            .set("targeted", targeted)
            .set("delivered", result.delivered)
            .set("expired", result.expired)
            .set("failed", result.failed),
    );
}

/// The one line that says the dispatch could not happen at all.
fn log_dispatch_failed(transition: &AgentPushTransition, error: &str) {
    roost_observability::log::warn(
        "push",
        "dispatch_failed",
        LogFields::new()
            .set("session_id", transition.session_id.as_str())
            .set("kind", transition.kind.as_str())
            .set("status_epoch", transition.status_epoch.as_str())
            .set("occupant_id", transition.occupant_id.as_str())
            .set("revision", transition.revision)
            .set("error", error),
    );
}
