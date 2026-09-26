//! The twelve event-admission rules, their order, and the two that look like
//! holes and are not.
//!
//! Covers: every rule in `apps/coord/src/events/event-admission.ts:29-122`,
//! driven with no database at all -- which is the point of the split. Also
//! covers the one predicate that decides public versus private, because a
//! divergence there is what makes a private recovery reference appear in a
//! dashboard.
//!
//! Two of these rules are counter-intuitive and both are load-bearing: a
//! deduplicated sequence is ADMITTED (so a retry can reach the claim path), and a
//! private reference for a force-closed session is ADMITTED (so the worker's
//! ordered replay is not wedged forever). Both are asserted explicitly.

// Every unwrap here is an assertion over a value the test just built: the panic
// IS the failure, which is why `unwrap_used` is denied in product code.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::events::admission::{AdmissionFacts, AdmissionRefusal, admit};
use roost_coord::events::visibility::{PRIVATE_SESSION_EVENT_KIND, kind_is_public};

/// A live worker caller, a new `opened` session: the shape almost every rule is
/// written against.
fn live_worker() -> AdmissionFacts {
    AdmissionFacts {
        caller_worker_fp: Some("aa".repeat(32)),
        caller_worker_live: true,
        event_kind: "opened".to_string(),
        ..AdmissionFacts::default()
    }
}

fn refused(facts: &AdmissionFacts) -> AdmissionRefusal {
    let decision = admit(facts);
    assert!(!decision.admitted, "expected a refusal, got {decision:?}");
    decision.refusal.expect("a refusal names its rule")
}

fn admitted(facts: &AdmissionFacts) -> bool {
    admit(facts).admitted
}

#[test]
fn rule_one_a_non_worker_producer_always_passes_and_only_probes_for_existence() {
    // The ghost-close and deploy-line path. Fencing it would break the
    // coordinator's recovery of its own records
    // (`apps/coord/src/sessions/handlers-sessions.ts:163,201,283`).
    let facts = AdmissionFacts {
        caller_worker_fp: None,
        event_kind: "closed".to_string(),
        session_id: Some("s-1".to_string()),
        session_exists: true,
        // Everything a worker caller would be refused for.
        caller_worker_live: false,
        session_row_worker_fp: Some("bb".repeat(32)),
        ..AdmissionFacts::default()
    };
    let decision = admit(&facts);
    assert!(decision.admitted);
    assert_eq!(decision.session_id.as_deref(), Some("s-1"));
    assert!(decision.session_exists);
}

#[test]
fn rule_two_a_tombstoned_or_absent_worker_is_refused_before_any_session_lookup() {
    // The order matters: a caller whose worker row is gone must not be able to
    // tell a foreign session from an absent one.
    let mut facts = live_worker();
    facts.caller_worker_live = false;
    facts.session_id = Some("someone-elses".to_string());
    facts.session_exists = true;
    assert_eq!(refused(&facts), AdmissionRefusal::CallerWorkerNotLive);
}

#[test]
fn rule_three_an_opened_or_snapshot_naming_another_worker_is_refused() {
    let mut facts = live_worker();
    facts.event_worker_fp = Some("cc".repeat(32));
    assert_eq!(refused(&facts), AdmissionRefusal::ForeignWorkerClaim);

    // The same `worker_fp` the caller authenticated as is fine.
    facts.event_worker_fp = Some("aa".repeat(32));
    assert!(admitted(&facts));
}

#[test]
fn rule_four_a_deduplicated_sequence_is_admitted_so_a_retry_can_be_claimed() {
    // NOT a hole. If this refused, the worker would replay a sequence forever and
    // the lost publication it is retrying to recover could never be claimed.
    let mut facts = live_worker();
    facts.already_deduplicated = true;
    // Everything below would be refused for a fresh sequence.
    facts.session_id = Some("s-1".to_string());
    facts.session_exists = false;
    assert!(
        admitted(&facts),
        "a dedupe must reach the claim path, not stop at admission"
    );
}

#[test]
fn rule_five_and_six_read_different_data_and_refuse_differently() {
    // Rule 5 is the snapshot's OWN claim about who owns each session; rule 6 is
    // the row already on disk. Same question, two sources, two refusal names --
    // because which one fired tells an operator whether the worker invented the
    // claim or collided with an existing row.
    let mut facts = live_worker();
    facts.event_kind = "snapshot".to_string();
    facts.snapshot_event_worker_fps = vec!["aa".repeat(32)];
    facts.snapshot_row_worker_fps = vec!["aa".repeat(32)];
    assert!(admitted(&facts));

    facts.snapshot_event_worker_fps = vec!["dd".repeat(32)];
    assert_eq!(refused(&facts), AdmissionRefusal::ForeignAnnouncedSession);

    facts.snapshot_event_worker_fps = vec!["aa".repeat(32)];
    facts.snapshot_row_worker_fps = vec!["dd".repeat(32)];
    assert_eq!(refused(&facts), AdmissionRefusal::ForeignAnnouncedRow);
}

#[test]
fn rule_seven_a_snapshot_naming_a_missing_workspace_is_refused() {
    let mut facts = live_worker();
    facts.event_kind = "snapshot".to_string();
    facts.snapshot_workspace_ids = vec!["w-1".to_string(), "w-2".to_string()];
    facts.existing_snapshot_workspace_count = 2;
    assert!(admitted(&facts));

    facts.existing_snapshot_workspace_count = 1;
    assert_eq!(refused(&facts), AdmissionRefusal::MissingSnapshotWorkspace);
}

#[test]
fn rule_eight_an_event_with_no_session_id_passes() {
    let mut facts = live_worker();
    facts.event_kind = "snapshot".to_string();
    facts.session_id = None;
    assert!(admitted(&facts));
}

#[test]
fn rule_nine_a_session_another_worker_owns_is_refused() {
    let mut facts = live_worker();
    facts.event_kind = "cwd".to_string();
    facts.session_id = Some("s-1".to_string());
    facts.session_exists = true;
    facts.session_row_worker_fp = Some("aa".repeat(32));
    assert!(admitted(&facts));

    facts.session_row_worker_fp = Some("ee".repeat(32));
    assert_eq!(refused(&facts), AdmissionRefusal::ForeignSession);
}

#[test]
fn rule_ten_a_private_reference_for_a_force_closed_session_is_admitted() {
    // NOT a hole, and the comment is the whole reason: "A reference queued before
    // an offline force-close must still be consumed or it permanently blocks the
    // worker's ordered durable replay"
    // (`apps/coord/src/events/event-admission.ts:108-120`).
    let mut facts = live_worker();
    facts.event_kind = PRIVATE_SESSION_EVENT_KIND.to_string();
    facts.session_id = Some("s-gone".to_string());
    facts.session_exists = false;
    facts.worker_has_prior_opened = true;
    assert!(admitted(&facts));

    // Without the prior durable `opened`, the same event is rule 11.
    facts.worker_has_prior_opened = false;
    assert_eq!(refused(&facts), AdmissionRefusal::UnknownSession);
}

#[test]
fn rule_eleven_an_unknown_session_for_anything_but_opened_is_refused() {
    for kind in [
        "cwd",
        "closed",
        "respawned",
        "renamed",
        "git",
        "pr",
        "ports",
    ] {
        let mut facts = live_worker();
        facts.event_kind = kind.to_string();
        facts.session_id = Some("s-gone".to_string());
        facts.session_exists = false;
        facts.worker_has_prior_opened = true;
        assert_eq!(
            refused(&facts),
            AdmissionRefusal::UnknownSession,
            "kind {kind}"
        );
    }
}

#[test]
fn rule_twelve_an_opened_for_a_genuinely_new_session_passes() {
    let mut facts = live_worker();
    facts.session_id = Some("s-new".to_string());
    facts.session_exists = false;
    let decision = admit(&facts);
    assert!(decision.admitted);
    assert_eq!(decision.session_id.as_deref(), Some("s-new"));
    assert!(!decision.session_exists);
}

#[test]
fn a_refusal_reports_no_session_existence_to_the_caller() {
    // The oracle property. `session_exists` is `false` on every refusal even when
    // the row is plainly there, so a prober cannot distinguish "absent" from
    // "not yours" by reading the answer
    // (`apps/coord/src/events/event-admission.ts:1-3`).
    let mut facts = live_worker();
    facts.event_kind = "cwd".to_string();
    facts.session_id = Some("s-1".to_string());
    facts.session_exists = true;
    facts.session_row_worker_fp = Some("ee".repeat(32));
    let decision = admit(&facts);
    assert!(!decision.admitted);
    assert!(
        !decision.session_exists,
        "a refusal must not leak existence"
    );
    assert_eq!(decision.session_id.as_deref(), Some("s-1"));
}

#[test]
fn the_worker_producer_and_the_non_worker_producer_never_see_the_same_verdict() {
    // A non-worker producer with identical facts is admitted where a worker
    // caller is refused. If these ever agreed, the ghost-close path would be
    // fenced by the worker rules.
    let mut worker = live_worker();
    worker.caller_worker_live = false;
    let mut internal = worker.clone();
    internal.caller_worker_fp = None;
    assert!(!admitted(&worker));
    assert!(admitted(&internal));
}

#[test]
fn exactly_one_event_kind_is_private_and_it_is_the_agent_reference() {
    // The single predicate every consumer shares
    // (`apps/coord/src/events/session-event-visibility.ts:7-18`). A second
    // private kind added here without updating the durable readers and the
    // publisher would leak it to every dashboard.
    assert_eq!(PRIVATE_SESSION_EVENT_KIND, "agent_reference");
    assert!(!kind_is_public(PRIVATE_SESSION_EVENT_KIND));
    for kind in [
        "opened",
        "closed",
        "attached",
        "detached",
        "cwd",
        "workspace_assigned",
        "snapshot",
        "respawned",
        "renamed",
        "git",
        "pr",
        "ports",
    ] {
        assert!(kind_is_public(kind), "{kind} should be public");
    }
}
