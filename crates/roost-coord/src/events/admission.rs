//! Whether a `SessionEvent` may be appended at all, as pure data with no
//! database behind it.
//!
//! Owned by the coordinator. The I/O layer reads these facts out of SQLite and
//! calls [`admit`]; nothing here touches a connection. That separation is the
//! point: these are twelve ordered decisions and each has a written reason, so
//! they belong somewhere a test can drive without a database.
//!
//! EVERY REFUSAL IS A DATA OUTCOME, NOT AN EXCEPTION. The source header
//! (`apps/coord/src/events/event-admission.ts:1-3`): "Worker-originated
//! resource probes fail as a data outcome, not an exception, so missing and
//! foreign IDs receive neither an ACK nor a socket-close oracle." A rejected
//! event gets **no acknowledgement and no close**, so a prober cannot tell
//! "never existed" from "not yours" from how the coordinator behaves. An
//! exception here would be a 1008 and an oracle for free.
//!
//! WHY THE ORDER MATTERS. Checks run foreign-cheapest first. A caller whose
//! worker row is gone is refused before any session lookup, so a tombstoned
//! worker cannot use the coordinator as a session-existence oracle by watching
//! which check fires.

use crate::events::visibility::PRIVATE_SESSION_EVENT_KIND;

/// What the caller claims to be, and what the database says.
///
/// Every field is one read the I/O layer already performs, named so the rule it
/// feeds is obvious from the field name. An empty vec where a rule expects
/// "every id is owned" means the caller supplied no ids, which passes -- that
/// is containment, not a coincidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdmissionFacts {
    /// The authenticated `worker_fp`, or `None` for a non-worker producer such
    /// as the ghost-close and deploy-line paths.
    pub caller_worker_fp: Option<String>,
    /// The `worker_fp` the event's own body claims, for `opened` and `snapshot`.
    pub event_worker_fp: Option<String>,
    /// A `workers` row exists for the caller and is not tombstoned.
    pub caller_worker_live: bool,
    /// A durable row already exists for `(caller_worker_fp, client_seq)`.
    pub already_deduplicated: bool,
    /// The event's own session id, when it has one.
    pub session_id: Option<String>,
    /// The event's `kind` discriminator.
    pub event_kind: String,
    /// A `sessions` row exists for `session_id`.
    pub session_exists: bool,
    /// The `worker_fp` on the existing `sessions` row, when there is one.
    pub session_row_worker_fp: Option<String>,
    /// The `worker_fp` each session inside a `snapshot` event claims, by the
    /// order the event lists them. Feeds rule 5.
    pub snapshot_event_worker_fps: Vec<String>,
    /// The `worker_fp` on each existing row for the announced session ids, in
    /// the same order. Feeds rule 6.
    pub snapshot_row_worker_fps: Vec<String>,
    /// The workspace ids a snapshot names.
    pub snapshot_workspace_ids: Vec<String>,
    /// How many of those workspaces exist.
    pub existing_snapshot_workspace_count: usize,
    /// Whether the coordinator holds a prior durable `opened` for `session_id`.
    pub worker_has_prior_opened: bool,
}

/// What admission decided, and the two facts the transaction needs next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Admission {
    /// Whether the event may be written. A refusal writes nothing at all.
    pub admitted: bool,
    /// The session the event belongs to, when it has one.
    pub session_id: Option<String>,
    /// Whether a session row existed before this transaction.
    pub session_exists: bool,
    /// Which rule refused it, for the log line. Never for the peer.
    pub refusal: Option<AdmissionRefusal>,
}

impl Admission {
    fn admit(session_id: Option<String>, session_exists: bool) -> Self {
        Self {
            admitted: true,
            session_id,
            session_exists,
            refusal: None,
        }
    }

    fn refuse(refusal: AdmissionRefusal, session_id: Option<String>) -> Self {
        Self {
            admitted: false,
            session_id,
            session_exists: false,
            refusal: Some(refusal),
        }
    }
}

/// Which rule refused an event. One variant per rule, so a log line names the
/// rule rather than saying "admission failed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionRefusal {
    /// Rule 2: the caller's worker row is gone or tombstoned.
    #[error("caller worker is not live")]
    CallerWorkerNotLive,
    /// Rule 3: an `opened` or `snapshot` naming another `worker_fp`.
    #[error("event claims another worker fingerprint")]
    ForeignWorkerClaim,
    /// Rule 5: a snapshot whose own session objects name another worker.
    #[error("snapshot announces a session the worker does not own")]
    ForeignAnnouncedSession,
    /// Rule 6: an announced id already exists and belongs to another worker.
    #[error("announced session already exists under another worker")]
    ForeignAnnouncedRow,
    /// Rule 7: a snapshot naming a workspace that does not exist.
    #[error("snapshot names a workspace that does not exist")]
    MissingSnapshotWorkspace,
    /// Rule 9: the event names a session another worker owns.
    #[error("session is owned by another worker")]
    ForeignSession,
    /// Rule 11: an unknown session for any kind but `opened`.
    #[error("unknown session")]
    UnknownSession,
}

/// Decide whether an event may be appended.
///
/// The rules, in the order the source runs them
/// (`apps/coord/src/events/event-admission.ts:29-122`):
///
/// 1. a **non-worker producer always passes**, probing only for existence. That
///    is the coordinator's own ghost-close and deploy-line path
///    (`sessions/handlers-sessions.ts:163,201,283`); fencing it would break the
///    coordinator's recovery of its own records.
/// 2. the caller's worker row must exist and not be tombstoned.
/// 3. `opened` and `snapshot` may not claim a `worker_fp` other than the caller's.
/// 4. an already-deduplicated `(worker, client_seq)` **passes** -- it must, or a
///    retry could never reach the claim path that publishes the lost effect.
/// 5. every session a snapshot's own body names belongs to the caller.
/// 6. every existing row for an announced id belongs to the caller.
/// 7. every workspace a snapshot names exists.
/// 8. an event with no session id passes -- that is a `snapshot`.
/// 9. session ownership.
/// 10. an `agent_reference` for a row that is gone, where the worker holds a
///     prior durable `opened`, **passes**: "A reference queued before an offline
///     force-close must still be consumed or it permanently blocks the worker's
///     ordered durable replay."
/// 11. an unknown session for any kind but `opened` is refused.
/// 12. `opened` for a genuinely new session passes.
#[must_use]
pub fn admit(facts: &AdmissionFacts) -> Admission {
    let session_id = facts.session_id.clone();
    let caller = facts.caller_worker_fp.as_deref();

    // Rule 1.
    if caller.is_none() {
        return Admission::admit(session_id, facts.session_exists);
    }

    // Rule 2.
    if !facts.caller_worker_live {
        return Admission::refuse(AdmissionRefusal::CallerWorkerNotLive, session_id);
    }

    // Rule 3.
    if let Some(claimed) = facts.event_worker_fp.as_deref()
        && Some(claimed) != caller
    {
        return Admission::refuse(AdmissionRefusal::ForeignWorkerClaim, session_id);
    }

    // Rule 4, admitted on purpose.
    if facts.already_deduplicated {
        return Admission::admit(session_id, false);
    }

    // Rules 5 and 6 read different data -- the event's own claim versus the
    // row already on disk -- so they are separate refusals even though both ask
    // "does this worker own that session".
    if any_foreign(
        facts.caller_worker_fp.as_deref(),
        &facts.snapshot_event_worker_fps,
    ) {
        return Admission::refuse(AdmissionRefusal::ForeignAnnouncedSession, session_id);
    }
    if any_foreign(
        facts.caller_worker_fp.as_deref(),
        &facts.snapshot_row_worker_fps,
    ) {
        return Admission::refuse(AdmissionRefusal::ForeignAnnouncedRow, session_id);
    }

    // Rule 7.
    if facts.existing_snapshot_workspace_count != facts.snapshot_workspace_ids.len() {
        return Admission::refuse(AdmissionRefusal::MissingSnapshotWorkspace, session_id);
    }

    // Rule 8.
    let Some(session_id) = session_id else {
        return Admission::admit(None, false);
    };

    // Rule 9.
    if facts.session_exists {
        let owned = facts.session_row_worker_fp.as_deref() == caller;
        return if owned {
            Admission::admit(Some(session_id), true)
        } else {
            Admission::refuse(AdmissionRefusal::ForeignSession, Some(session_id))
        };
    }

    // Rule 12, checked before rule 10 because `opened` is unconditional and the
    // reference rule is a narrow exception carved out of rule 11.
    if facts.event_kind == "opened" {
        return Admission::admit(Some(session_id), false);
    }

    // Rule 10.
    if facts.event_kind == PRIVATE_SESSION_EVENT_KIND && facts.worker_has_prior_opened {
        return Admission::admit(Some(session_id), false);
    }

    // Rule 11.
    Admission::refuse(AdmissionRefusal::UnknownSession, Some(session_id))
}

fn any_foreign(caller: Option<&str>, claimed: &[String]) -> bool {
    let Some(caller) = caller else {
        return false;
    };
    claimed.iter().any(|owner| owner != caller)
}
