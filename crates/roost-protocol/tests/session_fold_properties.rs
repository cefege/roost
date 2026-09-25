//! The properties the canonical fold owes every caller, whatever the variant.
//!
//! An event for a session the projection does not hold changes nothing, a
//! mutating variant never touches the map it was handed, a breadcrumb survives
//! a worker restart until the row is explicitly closed, and folding a log
//! incrementally is the same projection as folding it in one pass. What each
//! individual variant writes into a row is the other half, in `session_fold.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::json;

use roost_protocol::wire::event::{SessionMap, fold_all, fold_event};

use support::{
    FINGERPRINT, OTHER_SESSION, SESSION, event, opened_for, session_id, with_one_session,
};

#[test]
fn an_event_for_a_session_the_projection_does_not_hold_changes_nothing() {
    let opened = with_one_session();
    for unknown in [
        json!({ "kind": "cwd", "session_id": OTHER_SESSION, "cwd": "/x", "ts": 2 }),
        json!({ "kind": "workspace_assigned", "session_id": OTHER_SESSION, "workspace_id": null, "ts": 2 }),
        json!({ "kind": "respawned", "session_id": OTHER_SESSION, "new_channel": 3, "ts": 2 }),
        json!({ "kind": "renamed", "session_id": OTHER_SESSION, "custom_title": "x", "ts": 2 }),
        json!({ "kind": "git", "session_id": OTHER_SESSION, "branch": "main", "ts": 2 }),
        json!({ "kind": "pr", "session_id": OTHER_SESSION, "number": null, "state": null, "checks": null, "url": null, "ts": 2 }),
        json!({ "kind": "ports", "session_id": OTHER_SESSION, "ports": [1], "ts": 2 }),
    ] {
        let before = opened.clone();
        assert_eq!(fold_event(&opened, &event(unknown)), before);
    }
}

#[test]
fn a_mutating_event_never_touches_the_map_it_was_handed() {
    let opened = with_one_session();
    let before = opened.clone();
    let next = fold_event(
        &opened,
        &event(json!({ "kind": "cwd", "session_id": SESSION, "cwd": "/drifted", "ts": 2 })),
    );
    assert_eq!(opened, before, "the input map was mutated");
    assert_eq!(next.get(&session_id(SESSION)).unwrap().cwd, "/drifted");
}

#[test]
fn a_breadcrumb_survives_a_restart_until_it_is_explicitly_closed() {
    let restart = event(json!({
        "kind": "snapshot", "worker_fp": FINGERPRINT, "sessions": [], "ts": 2,
    }));
    let kept = fold_all(&[opened_for(SESSION, FINGERPRINT, 1), restart.clone()]);
    assert!(kept.contains_key(&session_id(SESSION)));

    let rebound = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        restart.clone(),
        event(json!({ "kind": "respawned", "session_id": SESSION, "new_channel": 7, "ts": 3 })),
    ]);
    assert_eq!(
        rebound.get(&session_id(SESSION)).unwrap().channel.as_u32(),
        7
    );

    let removed = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        restart,
        event(json!({ "kind": "closed", "session_id": SESSION, "exit_code": 0, "ts": 4 })),
    ]);
    assert!(!removed.contains_key(&session_id(SESSION)));
}

#[test]
fn folding_incrementally_is_the_same_projection_as_folding_in_one_pass() {
    let log = vec![
        opened_for(SESSION, FINGERPRINT, 1),
        opened_for(OTHER_SESSION, FINGERPRINT, 2),
        event(json!({ "kind": "renamed", "session_id": SESSION, "custom_title": "A", "ts": 3 })),
        event(json!({ "kind": "cwd", "session_id": OTHER_SESSION, "cwd": "/x", "ts": 4 })),
        event(json!({ "kind": "closed", "session_id": OTHER_SESSION, "exit_code": 1, "ts": 5 })),
        event(json!({ "kind": "attached", "session_id": SESSION, "ts": 6 })),
        event(json!({ "kind": "respawned", "session_id": SESSION, "new_channel": 4, "ts": 7 })),
    ];
    let batch = fold_all(&log);
    let mut incremental = SessionMap::new();
    for event in &log {
        incremental = fold_event(&incremental, event);
    }
    assert_eq!(incremental, batch);
    // Determinism: the same log folds to the same projection every time.
    assert_eq!(fold_all(&log), batch);
}
