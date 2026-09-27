//! Each close code says something different, and one of them says two different
//! things.
//!
//! A catch-all — "the socket closed, redial" — is wrong for three of the four
//! verdicts, and wrong in both directions that cost a user their terminal: an
//! immediate redial loop against a credential the coordinator has already
//! refused, and a backoff on a socket that was holding records this client could
//! have resumed from.
//!
//! `1013` is the reason this file is split out at all. The coordinator sends it
//! for two opposite verdicts — the connection was refused, or the application
//! window was exceeded — and only the reason string beside the code separates
//! them (`crates/roost-coord/src/sync_ws/upgrade_admission.rs:52-53` against
//! `ack_window.rs:42-44`).

use std::collections::BTreeSet;

use roost_client_core::client::sync::{
    AbortReason, CloseDisposition, SingleSyncLoop, classify_close,
};

#[test]
fn each_close_code_produces_its_own_handling() {
    let cases = [
        (
            Some(1013),
            "sync backpressure",
            CloseDisposition::Backpressure,
        ),
        (
            Some(1013),
            "connection rejected",
            CloseDisposition::ConnectionRejected,
        ),
        (
            Some(4000),
            "terminal liveness timeout",
            CloseDisposition::GenerationRetired,
        ),
        (Some(4001), "", CloseDisposition::AuthRevoked),
        (Some(1008), "invalid ack", CloseDisposition::Abrupt),
        (Some(1006), "", CloseDisposition::Abrupt),
        (None, "", CloseDisposition::Abrupt),
    ];
    for (code, reason, expected) in cases {
        assert_eq!(
            classify_close(code, reason),
            expected,
            "close {code:?} {reason:?} was read as something else"
        );
    }
    // Not one catch-all: the seven closes above are five dispositions.
    let distinct: BTreeSet<CloseDisposition> = cases
        .iter()
        .map(|(code, reason, _)| classify_close(*code, reason))
        .collect();
    assert_eq!(distinct.len(), 5);
}

#[test]
fn the_four_close_policies_are_four_policies() {
    // Backpressure: the records were HELD, so a redial resumes from the cursor
    // and a backoff would make the user wait for a socket that is fine.
    assert!(CloseDisposition::Backpressure.redials_immediately());
    assert!(CloseDisposition::Backpressure.preserved_records());
    assert_eq!(
        CloseDisposition::Backpressure.abort_reason(),
        Some(AbortReason::Flow)
    );

    // Connection rejection: the same code, the opposite meaning. An immediate
    // redial here is a loop against a coordinator that has already refused us.
    assert!(!CloseDisposition::ConnectionRejected.redials_immediately());
    assert!(!CloseDisposition::ConnectionRejected.preserved_records());
    assert_eq!(CloseDisposition::ConnectionRejected.abort_reason(), None);

    // Generation retired: our own close, and it redials once. The coordinator
    // cannot tell this from a tab that went away.
    assert!(CloseDisposition::GenerationRetired.redials_immediately());
    assert!(!CloseDisposition::GenerationRetired.preserved_records());

    // Revoked: terminal. A redial presents the same rejected credential.
    assert!(CloseDisposition::AuthRevoked.is_terminal());
    assert!(!CloseDisposition::AuthRevoked.redials_immediately());
    assert!(!CloseDisposition::AuthRevoked.preserved_records());

    // Abrupt is the only one that waits.
    assert!(!CloseDisposition::Abrupt.is_terminal());
    assert!(!CloseDisposition::Abrupt.redials_immediately());
    assert!(!CloseDisposition::Abrupt.preserved_records());
}

#[test]
fn the_reconnect_loop_starts_once() {
    // Two loops means two socket generations competing for one store, and the
    // loser is undetectable: both dials succeed and the store keeps whichever
    // link the second one installed.
    let mut loop_owner = SingleSyncLoop::new();
    let mut started = 0_u32;
    assert!(loop_owner.start(|| started += 1));
    assert!(!loop_owner.start(|| started += 1));
    assert_eq!(started, 1);
    assert!(loop_owner.has_started());
}

#[test]
fn only_the_five_abort_reasons_are_recognised() {
    for reason in AbortReason::ALL {
        assert_eq!(AbortReason::from_reason(reason.as_str()), Some(reason));
    }
    // The spellings are the wire spellings: a reason rides a close reason string
    // and a host's own log.
    assert_eq!(AbortReason::Visibility.as_str(), "visibility");
    assert_eq!(AbortReason::Manual.as_str(), "manual");
    assert_eq!(AbortReason::Stale.as_str(), "stale");
    assert_eq!(AbortReason::Flow.as_str(), "flow");
    assert_eq!(AbortReason::TerminalLiveness.as_str(), "terminal-liveness");
    // A network close carries no reason of ours, and gets the backoff.
    assert_eq!(AbortReason::from_reason("network"), None);
    assert_eq!(AbortReason::from_reason(""), None);
}
