// The workspace tree: what a delete takes with it, what a membership rewrite
// collects, and the order those two do their work in.
//
// The cascade order is the subject, not a detail. `workspace_sessions` cascades
// away with the workspace row, so the read that decides what a delete must detach
// has to happen BEFORE the delete, and the read that decides which workspaces a
// rewrite must collect has to happen AFTER it. A reordering of either is silent:
// the transaction still commits, and what it costs is a membership nobody owns
// rather than an error anybody sees.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workspaces_support;

use connectrpc::ErrorCode;
use roost_coord::sessions::rpc_workspaces::{
    handle_workspaces_create, handle_workspaces_delete, handle_workspaces_list,
    handle_workspaces_set_sessions, handle_workspaces_update,
};

use workspaces_support::{
    SESSION_A, SESSION_B, SESSION_C, WORKER_FP, WorkspacesFixture, device_caller,
};

/// Create a workspace and answer with its id.
async fn create(fixture: &WorkspacesFixture, folder: &str, sessions: &[&str]) -> String {
    handle_workspaces_create(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesCreateRequest {
            worker_fp: WORKER_FP.to_owned(),
            name: format!("ws-{folder}"),
            folder_path: folder.to_owned(),
            attach_session_ids: sessions.iter().map(|id| (*id).to_owned()).collect(),
            ..Default::default()
        },
    )
    .await
    .expect("a created workspace")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response")
    .id
}

/// The membership a delete takes with it, and the scope it stays inside.
///
/// A workspace holds sessions; deleting it must leave no row anywhere that still
/// names it, or the browser reads a session out of a workspace its next
/// re-fetch does not have. The second workspace is the boundary: the cascade is
/// the workspace's own descendants and nothing else.
#[tokio::test]
async fn deleting_a_workspace_leaves_no_dangling_membership() {
    let fixture = WorkspacesFixture::new("cascade").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    fixture.enroll_session(SESSION_B, "/srv/one").await;
    fixture.enroll_session(SESSION_C, "/srv/two").await;
    let doomed = create(&fixture, "/srv/one", &[SESSION_A, SESSION_B]).await;
    let survivor = create(&fixture, "/srv/two", &[SESSION_C]).await;
    assert_eq!(fixture.junction_rows(&doomed).await, 2);
    assert_eq!(fixture.session_workspace_id(SESSION_A).await, doomed);

    handle_workspaces_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesDeleteRequest {
            id: doomed.clone(),
            if_version: 0,
        },
    )
    .await
    .expect("a deleted workspace");

    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workspaces WHERE id = '{doomed}'"
            ))
            .await,
        0,
        "the parent row is gone"
    );
    assert_eq!(fixture.junction_rows(&doomed).await, 0, "its junction cascaded away");
    for session_id in [SESSION_A, SESSION_B] {
        assert_eq!(
            fixture.session_workspace_id(session_id).await,
            "",
            "{session_id} is a true orphan, not a pointer at a deleted row"
        );
    }
    assert_eq!(
        fixture.session_workspace_id(SESSION_C).await,
        survivor,
        "another workspace's sessions are not part of this cascade"
    );
    assert_eq!(fixture.junction_rows(&survivor).await, 1);
}

/// A mid-cascade failure rolls the whole cascade back.
///
/// The detach runs BEFORE the delete, so the failure that matters is the delete
/// refusing after the sessions have already been ungrouped. A trigger is the
/// only way to make a delete fail on demand: without one, the rollback this
// proves is untestable, and a half-deleted tree is exactly what a rolled-back
/// detach is protecting the database from.
#[tokio::test]
async fn a_failed_delete_leaves_no_half_deleted_tree() {
    let fixture = WorkspacesFixture::new("rollback").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    fixture.enroll_session(SESSION_B, "/srv/one").await;
    let doomed = create(&fixture, "/srv/one", &[SESSION_A, SESSION_B]).await;
    fixture
        .exec("CREATE TRIGGER refuse_workspace_delete BEFORE DELETE ON workspaces \
               BEGIN SELECT RAISE(ABORT, 'delete refused'); END")
        .await;

    let refused = handle_workspaces_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesDeleteRequest {
            id: doomed.clone(),
            if_version: 0,
        },
    )
    .await
    .expect_err("the trigger refuses the delete");
    assert_eq!(refused.code, ErrorCode::Internal);

    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workspaces WHERE id = '{doomed}'"
            ))
            .await,
        1,
        "the workspace is still there"
    );
    assert_eq!(fixture.junction_rows(&doomed).await, 2, "its membership is intact");
    for session_id in [SESSION_A, SESSION_B] {
        assert_eq!(
            fixture.session_workspace_id(session_id).await,
            doomed,
            "{session_id} is still grouped: the detach rolled back with the delete"
        );
    }
    assert_eq!(
        fixture.recorded().len(),
        1,
        "only the create announced anything: a refused delete publishes nothing"
    );
}

/// THE CASCADE ORDER, IN THE DIRECTION THAT IS EASY TO GET WRONG.
///
/// A rewrite empties the workspaces it takes sessions FROM, and collects the
/// ones that end up empty. The emptiness read must come after the rewrite: read
/// before it, the source workspace still looks occupied by the session that is
/// about to leave, so it is not collected -- and worse, a collector that reads
/// emptiness from a pre-rewrite junction and deletes on it takes a LIVE parent
/// with it and cascades away a session that never moved.
#[tokio::test]
async fn a_workspace_that_still_holds_a_session_is_not_collected() {
    let fixture = WorkspacesFixture::new("order").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    fixture.enroll_session(SESSION_B, "/srv/one").await;
    let target = create(&fixture, "/srv/target", &[]).await;
    let source = create(&fixture, "/srv/source", &[SESSION_A, SESSION_B]).await;

    let rewritten = handle_workspaces_set_sessions(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesSetSessionsRequest {
            id: target.clone(),
            if_version: 0,
            session_ids: vec![SESSION_A.to_owned()],
        },
    )
    .await
    .expect("a rewritten membership")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");

    assert_eq!(rewritten.session_ids, vec![SESSION_A.to_owned()]);
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workspaces WHERE id = '{source}'"
            ))
            .await,
        1,
        "the source still holds SESSION_B, so the collector must leave it alone"
    );
    assert_eq!(fixture.junction_rows(&source).await, 1);
    assert_eq!(
        fixture.session_workspace_id(SESSION_B).await,
        source,
        "the session that stayed keeps its workspace"
    );
    assert_eq!(fixture.session_workspace_id(SESSION_A).await, target);
    assert_eq!(fixture.junction_rows(&target).await, 1);
}

/// The collector's positive case, and the response a client gets for a target
/// that its own rewrite deleted.
#[tokio::test]
async fn a_workspace_that_loses_its_last_session_is_collected() {
    let fixture = WorkspacesFixture::new("collect").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    let target = create(&fixture, "/srv/target", &[]).await;
    let source = create(&fixture, "/srv/source", &[SESSION_A]).await;

    let emptied = handle_workspaces_set_sessions(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesSetSessionsRequest {
            id: target.clone(),
            if_version: 0,
            session_ids: vec![SESSION_A.to_owned()],
        },
    )
    .await
    .expect("a rewritten membership")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");
    assert_eq!(emptied.session_ids, vec![SESSION_A.to_owned()]);
    assert_eq!(
        fixture.session_workspace_id(SESSION_A).await,
        target,
        "the session that moved follows it, on both representations"
    );
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workspaces WHERE id = '{source}'"
            ))
            .await,
        0,
        "the source lost its last session and went with it"
    );

    let gone = handle_workspaces_set_sessions(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesSetSessionsRequest {
            id: target.clone(),
            if_version: 1,
            session_ids: Vec::new(),
        },
    )
    .await
    .expect("a rewrite that empties its own target")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");
    assert_eq!(
        gone.session_ids,
        Vec::<String>::new(),
        "a target the collector deleted reports no members, not a phantom row"
    );
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workspaces WHERE id = '{target}'"
            ))
            .await,
        0
    );
    assert_eq!(
        fixture.session_workspace_id(SESSION_A).await,
        "",
        "the collected target took its last session's column with it: a true orphan, \
         not a pointer at a row that is gone"
    );
    assert_eq!(fixture.junction_rows(&target).await, 0);
}

/// The list's order is a property of the store, not of the query a caller wrote.
#[tokio::test]
async fn the_list_orders_by_position_then_by_id() {
    let fixture = WorkspacesFixture::new("order-list").await;
    let first = create(&fixture, "/srv/a", &[]).await;
    let second = create(&fixture, "/srv/b", &[]).await;
    let third = create(&fixture, "/srv/c", &[]).await;
    // Two rows on one position is the state a delete leaves behind, and the
    // tie-break is what stops the order from being whatever SQLite felt like.
    handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: third.clone(),
            if_version: 0,
            position: Some(0),
            ..Default::default()
        },
    )
    .await
    .expect("a reordered workspace");
    // The tied pair's ids are rewritten so that id order and insertion order
    // DISAGREE. Without that, a store that ordered by `position` alone would
    // agree with this test about half the time, and a mutation experiment needs
    // a detector that fails every time.
    for (id, replacement) in [
        (&first, "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"),
        (&third, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"),
    ] {
        fixture
            .exec(&format!("UPDATE workspaces SET id = '{replacement}' WHERE id = '{id}'"))
            .await;
    }

    let listed: Vec<(u32, String)> = handle_workspaces_list(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesListRequest::default(),
    )
    .await
    .expect("the list")
    .body
    .workspaces
    .into_iter()
    .map(|workspace| (workspace.position, workspace.id))
    .collect();
    let mut by_key = listed.clone();
    by_key.sort();
    assert_eq!(
        listed, by_key,
        "the answer is sorted by (position, id), whatever the ids happen to be"
    );
    assert_eq!(
        listed.iter().map(|(position, _)| *position).collect::<Vec<_>>(),
        vec![0, 0, 1],
        "`first` and `third` are tied at 0 and `second` was never moved"
    );
    assert_eq!(listed[2].1, second, "the untied row sorts last");
    assert_eq!(
        listed[0].1, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "of two rows on one position, the lower id sorts first -- not the earlier one"
    );
}
