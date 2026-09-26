//! The pairing approval lifecycle's rules, as a test rather than a reviewer.
//!
//! What is guarded here is the property that makes the ceremony safe: a
//! request nobody approved cannot be completed, a decided request never
//! re-opens, and the approver's status read discloses nothing to anybody but
//! the approver. Each of those is a refusal that a refactor can quietly widen,
//! so each has a named test that fails the moment it is widened.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::auth::pairing::status::{
    ApprovalAuthority, ApprovalOutcome, ApprovalStatusFacts, ApprovedRequest, LiveRequest,
    RequestIdentity, StoredStatus, TerminalRequest, read_approval_status,
};
use roost_coord::auth::pairing::{PairingRefusal, secrets::PAIR_VERIFICATION_ATTEMPT_LIMIT};

const NOW: i64 = 1_700_000_000_000;
const HANDLE: &str = "00112233445566778899aabbccddeeff";
const CODE_HASH: &str = "digest-of-the-code-the-approver-chose";

fn identity(expires_at_ms: i64) -> RequestIdentity {
    RequestIdentity {
        id: 7,
        ephemeral_id: HANDLE.to_string(),
        expires_at_ms,
    }
}

fn authority() -> ApprovalAuthority {
    ApprovalAuthority {
        account_id: "acct-1".to_string(),
        approver_fingerprint: Some("aa".repeat(32)),
    }
}

fn approved(attempts: i64, code_hash: Option<&str>) -> ApprovedRequest {
    ApprovedRequest {
        identity: identity(NOW + 1_000),
        approved_account_id: Some("acct-1".to_string()),
        approved_by_fingerprint: Some("aa".repeat(32)),
        verification_code_hash: code_hash.map(str::to_string),
        verification_attempts: attempts,
    }
}

fn pending() -> LiveRequest {
    LiveRequest::AwaitingApproval(identity(NOW + 1_000))
}

fn awaiting(attempts: i64, code_hash: Option<&str>) -> LiveRequest {
    LiveRequest::AwaitingConfirmation(approved(attempts, code_hash))
}

fn facts(status: StoredStatus, expires_at_ms: i64) -> ApprovalStatusFacts {
    ApprovalStatusFacts {
        status,
        approved_by_fingerprint: Some("aa".repeat(32)),
        expires_at_ms,
    }
}

/// A request nobody approved cannot be completed. THE load-bearing rule of the
/// ceremony: approval is a progression with a real last step, and the code that
/// performs the last step is only reachable from a request that has been
/// approved.
#[test]
fn a_pending_request_cannot_be_confirmed() {
    assert_eq!(
        pending().into_approved().unwrap_err(),
        PairingRefusal::NotAwaitingVerification
    );
}

/// And the refusal is a stage refusal, not a missing row: the requester proved
/// its token, so `NotFound` here would contradict the Poll that just told it
/// the row exists.
#[test]
fn an_unapproved_request_is_refused_as_a_stage_not_as_a_missing_row() {
    assert_eq!(
        PairingRefusal::NotAwaitingVerification.code(),
        connectrpc::ErrorCode::FailedPrecondition
    );
}

/// Approval is the only door onto the confirmation transitions, and it opens
/// once per request.
#[test]
fn approval_binds_a_code_once_and_only_from_pending() {
    let outcome = pending().approve(&authority(), CODE_HASH, NOW);
    assert!(matches!(outcome, ApprovalOutcome::Approved { .. }));
    let already = awaiting(0, Some(CODE_HASH)).approve(&authority(), CODE_HASH, NOW);
    assert!(matches!(already, ApprovalOutcome::Retry));
}

/// A second approval with a DIFFERENT code is somebody else's guess, and a
/// second approval by a DIFFERENT approver is a takeover. Both are refused
/// rather than treated as a retry, because either would re-point the request at
/// whoever presented last.
#[test]
fn a_second_approval_only_retries_the_approval_that_already_happened() {
    let wrong_code = awaiting(0, Some(CODE_HASH)).approve(&authority(), "other-digest", NOW);
    assert!(matches!(
        wrong_code,
        ApprovalOutcome::Refused(PairingRefusal::AlreadyApproved)
    ));
    let other_approver = ApprovalAuthority {
        account_id: "acct-1".to_string(),
        approver_fingerprint: Some("bb".repeat(32)),
    };
    let takeover = awaiting(0, Some(CODE_HASH)).approve(&other_approver, CODE_HASH, NOW);
    assert!(matches!(
        takeover,
        ApprovalOutcome::Refused(PairingRefusal::AlreadyApproved)
    ));
    let other_account = ApprovalAuthority {
        account_id: "acct-2".to_string(),
        approver_fingerprint: Some("aa".repeat(32)),
    };
    let moved = awaiting(0, Some(CODE_HASH)).approve(&other_account, CODE_HASH, NOW);
    assert!(matches!(
        moved,
        ApprovalOutcome::Refused(PairingRefusal::AlreadyApproved)
    ));
}

/// Expiry beats the status a row is stored with. A `pending` row past its
/// deadline is `expired` to every reader, which is why every caller asks
/// `is_expired_at` before it decides anything.
#[test]
fn expiry_outranks_a_stored_live_status() {
    let outcome = LiveRequest::AwaitingApproval(identity(NOW)).approve(&authority(), CODE_HASH, NOW);
    assert!(matches!(outcome, ApprovalOutcome::Expired));
    assert!(LiveRequest::AwaitingApproval(identity(NOW)).is_expired_at(NOW));
    assert!(!LiveRequest::AwaitingApproval(identity(NOW + 1)).is_expired_at(NOW));
}

/// A decided request is decided forever. Every terminal status refuses to
/// become a live value, which is what stops an approved-and-denied row from
/// being confirmed by a code an attacker kept.
#[test]
fn a_decided_request_never_becomes_live_again() {
    for terminal in [
        TerminalRequest::Denied,
        TerminalRequest::Expired,
        TerminalRequest::VerificationFailed,
        TerminalRequest::Completed,
    ] {
        let stored = StoredStatus::parse(terminal.as_wire()).unwrap();
        assert!(!stored.is_live(), "{terminal:?} must not be live");
    }
    assert!(StoredStatus::Pending.is_live());
    assert!(StoredStatus::VerificationRequired.is_live());
}

/// The attempt series ends at the bound and STOPS there, because the durable
/// column is what the next attempt reads and a counter above the limit would
/// report a longer series than the one that ended.
#[test]
fn the_attempt_series_ends_at_the_bound_and_saturates() {
    // `next_attempt` is relative to the count ALREADY STORED, so a row holding
    // `n` wrong answers hands back `n + 1`. Reading that as an absolute count
    // is how an off-by-one gets shipped twice: once in the port and once in the
    // test that was written to match it.
    for stored in 0..PAIR_VERIFICATION_ATTEMPT_LIMIT - 1 {
        let record = approved(stored, Some(CODE_HASH)).next_attempt();
        assert_eq!(
            record.attempts,
            stored + 1,
            "a row holding {stored} wrong answers must hand back {}",
            stored + 1
        );
        assert!(
            !record.exhausted,
            "after {stored} of {PAIR_VERIFICATION_ATTEMPT_LIMIT} the series must still be open"
        );
    }
    let final_attempt = approved(PAIR_VERIFICATION_ATTEMPT_LIMIT - 1, Some(CODE_HASH)).next_attempt();
    assert_eq!(final_attempt.attempts, PAIR_VERIFICATION_ATTEMPT_LIMIT);
    assert!(final_attempt.exhausted);
    let past = approved(PAIR_VERIFICATION_ATTEMPT_LIMIT + 9, Some(CODE_HASH)).next_attempt();
    assert_eq!(past.attempts, PAIR_VERIFICATION_ATTEMPT_LIMIT);
}

/// The approver's status read admits exactly the approver, and answers a
/// request that does not exist and a request somebody else approved with the
/// SAME refusal, so the read cannot be used to discover that an id exists.
#[test]
fn the_status_read_discloses_nothing_to_a_foreign_caller() {
    let now = NOW;
    let live = facts(StoredStatus::VerificationRequired, now + 1_000);
    assert_eq!(
        read_approval_status(Some(&live), Some(&"aa".repeat(32)), false, now).unwrap(),
        "verification_required"
    );
    assert_eq!(
        read_approval_status(Some(&live), Some(&"bb".repeat(32)), false, now).unwrap_err(),
        PairingRefusal::NotFound
    );
    assert_eq!(
        read_approval_status(Some(&live), None, false, now).unwrap_err(),
        PairingRefusal::NotFound
    );
    let absent = read_approval_status(None, Some(&"bb".repeat(32)), false, now).unwrap_err();
    let foreign = read_approval_status(Some(&live), Some(&"bb".repeat(32)), false, now).unwrap_err();
    assert_eq!(absent, foreign);
}

/// A direct on-host approval records no approving key, and that is precisely
/// who may read it back: a host approval is not attributable to a device, so
/// attributing it to one would let that device retire somebody else's approval.
#[test]
fn a_host_approval_is_readable_only_from_the_host() {
    let now = NOW;
    let host_approved = ApprovalStatusFacts {
        status: StoredStatus::VerificationRequired,
        approved_by_fingerprint: None,
        expires_at_ms: now + 1_000,
    };
    assert_eq!(
        read_approval_status(Some(&host_approved), Some(&"aa".repeat(32)), true, now).unwrap(),
        "verification_required"
    );
    assert_eq!(
        read_approval_status(Some(&host_approved), Some(&"aa".repeat(32)), false, now).unwrap_err(),
        PairingRefusal::NotFound
    );
}

/// The read normalizes an overdue live row to `expired` WITHOUT writing, and
/// says so on every live status rather than only the one it happened to check
/// in v2.
#[test]
fn an_overdue_read_reports_expired_for_every_live_status() {
    let now = NOW;
    for live in [StoredStatus::Pending, StoredStatus::VerificationRequired] {
        let overdue = facts(live, now);
        assert_eq!(
            read_approval_status(Some(&overdue), Some(&"aa".repeat(32)), false, now).unwrap(),
            "expired"
        );
        assert_eq!(
            read_approval_status(Some(&overdue), Some(&"aa".repeat(32)), false, now - 1).unwrap(),
            live.as_wire()
        );
    }
}

/// A status this coordinator never writes is a database fault naming itself,
/// not a value that falls through every comparison as "not live".
#[test]
fn an_unrecognised_stored_status_is_refused_by_name() {
    let error = StoredStatus::parse("approved").unwrap_err();
    assert!(
        error.reason.contains("approved"),
        "the refusal must name the value it refused, got {}",
        error.reason
    );
}
