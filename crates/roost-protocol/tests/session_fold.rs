//! The canonical fold, one test per variant: what each kind writes into a row,
//! and what it must leave alone.
//!
//! The properties every caller depends on — an unknown session changes
//! nothing, a mutating variant never touches the map it was handed, a
//! breadcrumb survives a restart, and folding a log incrementally is the same
//! projection as folding it in one pass — are in `session_fold_properties.rs`.
// A behaviour test unwraps the value it is asserting about: a failure
// there is the assertion failing, which is exactly what a test wants. The
// workspace denies `unwrap`/`expect` because a panic on a bad wire value in
// a running component is a fleet-visible outage, and that reasoning does not
// reach a test.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use serde_json::{Value, json};

use roost_protocol::wire::event::{fold_all, fold_event};

use support::{
    FINGERPRINT, OTHER_FINGERPRINT, OTHER_SESSION, SESSION, THIRD_SESSION, event, opened_for,
    session_id, with_one_session,
};

fn row(session: &str, worker: &str) -> Value {
    json!({
        "id": session,
        "worker_fp": worker,
        "channel": 1,
        "kind": "shell",
        "cwd": "/repo",
        "workspace_id": null,
        "status": "open",
        "created_at": 1,
        "closed_at": null,
        "custom_title": null,
    })
}

#[test]
fn opened_seeds_a_row_and_leaves_the_unresolved_fields_absent() {
    let map = with_one_session();
    let session = map.get(&session_id(SESSION)).expect("the opened row");
    assert_eq!(session.spawn_cwd.as_deref(), Some("/repo"));
    assert_eq!(session.cwd, "/repo");
    assert_eq!(session.workspace_id, None);
    assert_eq!(session.status.as_str(), "open");
    assert_eq!(session.created_at, 1);
    assert_eq!(session.closed_at, None);
    assert_eq!(session.custom_title, None);
    // Absent, not null: nothing has resolved the repository yet.
    assert_eq!(session.git_branch, None);
    assert_eq!(session.git_remote, None);
    assert_eq!(session.pr_number, None);
    assert_eq!(session.ports, None);
}

#[test]
fn opened_ignores_whatever_the_projection_already_held() {
    let map = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        event(
            json!({ "kind": "renamed", "session_id": SESSION, "custom_title": "First", "ts": 2 }),
        ),
        opened_for(SESSION, FINGERPRINT, 3),
    ]);
    let session = map.get(&session_id(SESSION)).expect("the reopened row");
    assert_eq!(session.created_at, 3);
    assert_eq!(session.custom_title, None);
}

#[test]
fn closed_is_the_only_deletion_trigger_and_is_a_no_op_for_an_unknown_session() {
    let map = fold_all(&[event(json!({
        "kind": "closed", "session_id": OTHER_SESSION, "exit_code": 0, "ts": 1,
    }))]);
    assert!(
        map.is_empty(),
        "a close nobody announced must not delete anything"
    );

    let opened = with_one_session();
    let after = fold_event(
        &opened,
        &event(json!({ "kind": "closed", "session_id": SESSION, "exit_code": 0, "ts": 2 })),
    );
    assert!(after.is_empty());
}

#[test]
fn attached_and_detached_leave_the_projection_alone() {
    let opened = with_one_session();
    for kind in ["attached", "detached"] {
        let noop = event(json!({ "kind": kind, "session_id": SESSION, "ts": 2 }));
        assert_eq!(
            fold_event(&opened, &noop),
            opened,
            "{kind} changed the projection"
        );
    }
}

#[test]
fn an_agent_reference_is_never_projected_into_public_session_state() {
    let opened = with_one_session();
    let noop = event(json!({
        "kind": "agent_reference",
        "session_id": SESSION,
        "reference": { "schema_version": 1, "agent_id": "omp", "kind": "id", "value": "agent-1" },
        "ts": 2,
    }));
    assert_eq!(fold_event(&opened, &noop), opened);
}

#[test]
fn a_cwd_event_drifts_the_live_folder_and_leaves_the_spawn_folder_alone() {
    let map = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        event(json!({ "kind": "cwd", "session_id": SESSION, "cwd": "/repo/apps/web", "ts": 2 })),
    ]);
    let session = map.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.cwd, "/repo/apps/web");
    assert_eq!(session.spawn_cwd.as_deref(), Some("/repo"));
}

#[test]
fn a_workspace_assignment_round_trips_through_the_orphan_bucket() {
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let assigned = fold_event(
        &with_one_session(),
        &event(json!({
            "kind": "workspace_assigned", "session_id": SESSION, "workspace_id": workspace, "ts": 2,
        })),
    );
    assert_eq!(
        assigned
            .get(&session_id(SESSION))
            .unwrap()
            .workspace_id
            .as_ref()
            .map(|id| id.as_str()),
        Some(workspace)
    );
    let orphaned = fold_event(
        &assigned,
        &event(json!({
            "kind": "workspace_assigned", "session_id": SESSION, "workspace_id": null, "ts": 3,
        })),
    );
    assert_eq!(
        orphaned.get(&session_id(SESSION)).unwrap().workspace_id,
        None
    );
}

#[test]
fn a_snapshot_preserves_exactly_four_fields_and_never_deletes() {
    let workspace = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    let before = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        event(json!({ "kind": "renamed", "session_id": SESSION, "custom_title": "Mine", "ts": 2 })),
        event(json!({
            "kind": "workspace_assigned", "session_id": SESSION, "workspace_id": workspace, "ts": 3,
        })),
        event(json!({ "kind": "cwd", "session_id": SESSION, "cwd": "/elsewhere", "ts": 4 })),
    ]);

    // The worker announces the same session from its own memory: it knows
    // neither the rename nor the workspace, and it re-reports its own folder.
    let mut announced = row(SESSION, FINGERPRINT);
    announced["cwd"] = json!("/elsewhere");
    announced["channel"] = json!(9);
    announced["status"] = json!("open");
    let after = fold_event(
        &before,
        &event(json!({
            "kind": "snapshot", "worker_fp": FINGERPRINT, "sessions": [announced], "ts": 5,
        })),
    );
    let session = after.get(&session_id(SESSION)).expect("the row survives");
    assert_eq!(session.created_at, 1, "created_at is carried over");
    assert_eq!(
        session.workspace_id.as_ref().map(|id| id.as_str()),
        Some(workspace),
        "workspace_id is carried over"
    );
    assert_eq!(
        session.custom_title.as_deref(),
        Some("Mine"),
        "custom_title is carried over"
    );
    assert_eq!(
        session.spawn_cwd.as_deref(),
        Some("/repo"),
        "spawn_cwd is carried over"
    );
    // Everything else is the worker's to overwrite.
    assert_eq!(session.channel.as_u32(), 9);
    assert_eq!(session.cwd, "/elsewhere");

    // A session of this worker missing from the snapshot is a breadcrumb, not
    // a deletion, and another worker's rows are not this worker's business.
    let two = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        opened_for(OTHER_SESSION, FINGERPRINT, 2),
        opened_for(THIRD_SESSION, OTHER_FINGERPRINT, 3),
    ]);
    let after = fold_event(
        &two,
        &event(json!({
            "kind": "snapshot", "worker_fp": FINGERPRINT, "sessions": [], "ts": 6,
        })),
    );
    assert_eq!(after.len(), 3, "a restart snapshot prunes nothing");
}

#[test]
fn a_respawn_rebinds_the_channel_and_forces_the_row_open() {
    let map = fold_all(&[
        opened_for(SESSION, FINGERPRINT, 1),
        event(json!({ "kind": "respawned", "session_id": SESSION, "new_channel": 7, "ts": 2 })),
    ]);
    let session = map.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.channel.as_u32(), 7);
    assert_eq!(session.status.as_str(), "open");
    assert_eq!(session.closed_at, None);
    assert_eq!(session.cwd, "/repo", "a respawn does not move the session");
}

#[test]
fn a_rename_is_sticky_and_an_empty_title_clears_it() {
    let renamed = fold_event(
        &with_one_session(),
        &event(
            json!({ "kind": "renamed", "session_id": SESSION, "custom_title": "Keep Me", "ts": 2 }),
        ),
    );
    let drifted = fold_event(
        &renamed,
        &event(json!({ "kind": "cwd", "session_id": SESSION, "cwd": "/elsewhere", "ts": 3 })),
    );
    let session = drifted.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.custom_title.as_deref(), Some("Keep Me"));
    assert_eq!(session.cwd, "/elsewhere");

    let cleared = fold_event(
        &drifted,
        &event(json!({ "kind": "renamed", "session_id": SESSION, "custom_title": "", "ts": 4 })),
    );
    assert_eq!(
        cleared.get(&session_id(SESSION)).unwrap().custom_title,
        None
    );
}

#[test]
fn a_git_event_without_a_remote_leaves_the_resolved_one_alone() {
    let resolved = fold_event(
        &with_one_session(),
        &event(json!({
            "kind": "git", "session_id": SESSION, "branch": "main", "remote": "owner/repo", "ts": 2,
        })),
    );
    let session = resolved.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.git_branch.as_deref(), Some("main"));
    assert_eq!(session.git_remote, Some(Some("owner/repo".to_owned())));

    // The branch is always reported; the remote only when it resolved again.
    let branch_only = fold_event(
        &resolved,
        &event(json!({ "kind": "git", "session_id": SESSION, "branch": "fix/1", "ts": 3 })),
    );
    let session = branch_only.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.git_branch.as_deref(), Some("fix/1"));
    assert_eq!(
        session.git_remote,
        Some(Some("owner/repo".to_owned())),
        "an absent remote must not clear the resolved one"
    );

    // A null branch is "not a repo", which is different from "not looked yet".
    let not_a_repo = fold_event(
        &branch_only,
        &event(json!({ "kind": "git", "session_id": SESSION, "branch": null, "ts": 4 })),
    );
    assert_eq!(
        not_a_repo.get(&session_id(SESSION)).unwrap().git_branch,
        None
    );
}

#[test]
fn a_pull_request_event_reports_every_field_together() {
    let map = fold_event(
        &with_one_session(),
        &event(json!({
            "kind": "pr",
            "session_id": SESSION,
            "number": 123,
            "state": "open",
            "checks": "passing",
            "url": "https://example.test/pr/123",
            "ts": 2,
        })),
    );
    let session = map.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.pr_number, Some(123));
    assert_eq!(session.pr_state.map(|state| state.as_str()), Some("open"));
    assert_eq!(
        session.pr_checks.map(|checks| checks.as_str()),
        Some("passing")
    );
    assert_eq!(
        session.pr_url.as_deref(),
        Some("https://example.test/pr/123")
    );

    let closed = fold_event(
        &map,
        &event(json!({
            "kind": "pr", "session_id": SESSION,
            "number": null, "state": null, "checks": null, "url": null, "ts": 3,
        })),
    );
    let session = closed.get(&session_id(SESSION)).expect("the row");
    assert_eq!(session.pr_number, None);
    assert_eq!(session.pr_state, None);
    assert_eq!(session.pr_checks, None);
    assert_eq!(session.pr_url, None);
}

#[test]
fn a_ports_event_replaces_the_whole_list() {
    let listening = fold_event(
        &with_one_session(),
        &event(json!({ "kind": "ports", "session_id": SESSION, "ports": [5174, 8080], "ts": 2 })),
    );
    assert_eq!(
        listening.get(&session_id(SESSION)).unwrap().ports,
        Some(vec![5174, 8080])
    );
    let quiet = fold_event(
        &listening,
        &event(json!({ "kind": "ports", "session_id": SESSION, "ports": [], "ts": 3 })),
    );
    assert_eq!(quiet.get(&session_id(SESSION)).unwrap().ports, Some(vec![]));
}
