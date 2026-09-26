// A workspace change reaching a subscriber, and the subscriber's answer being
// the same one a re-fetch gives.
//
// The subscriber here is a listener on `buses.workspace_bus`, which is the seam
// the Sync firehose's workspace adapter subscribes to: one `WorkspaceDelta` in,
// one firehose frame out, on the workspaces lane. A delta nobody receives is a
// committed write a browser never hears about, and a delta that disagrees with
// the list is a browser that shows a different sidebar depending on whether it
// re-fetched or listened -- so both halves are asserted here, against the same
// write.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workspaces_support;

use roost_coord::sessions::rpc_workspaces::{
    handle_workspaces_create, handle_workspaces_delete, handle_workspaces_list,
    handle_workspaces_set_sessions, handle_workspaces_update,
};
use roost_protocol::wire::{WorkspaceDelta, WorkspaceId};

use workspaces_support::{SESSION_A, SESSION_B, WORKER_FP, WorkspacesFixture, device_caller};

/// A workspace id back as the wire brand, for asserting a delta's payload.
fn workspace_id(value: &str) -> WorkspaceId {
    WorkspaceId::try_from(value).expect("a workspace id the brand accepts")
}

/// Create a workspace and answer with its proto row.
async fn create(
    fixture: &WorkspacesFixture,
    folder: &str,
    sessions: &[&str],
) -> roost_proto::Workspace {
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
}

/// Every one of the four mutations reaches a subscriber, in the order the calls
/// were made, and each carries the shape its change implies.
#[tokio::test]
async fn every_mutation_reaches_a_sync_subscriber() {
    let fixture = WorkspacesFixture::new("deltas").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    fixture.enroll_session(SESSION_B, "/srv/two").await;
    let target = create(&fixture, "/srv/two", &[]).await;
    let source = create(&fixture, "/srv/one", &[SESSION_A]).await;

    handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: target.id.clone(),
            if_version: 0,
            color: Some("amber".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("an updated workspace");
    handle_workspaces_set_sessions(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesSetSessionsRequest {
            id: target.id.clone(),
            if_version: 1,
            session_ids: vec![SESSION_A.to_owned()],
            ..Default::default()
        },
    )
    .await
    .expect("a rewritten membership");
    handle_workspaces_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesDeleteRequest {
            id: source.id.clone(),
            if_version: 0,
            ..Default::default()
        },
    )
    .await
    .expect("a deleted workspace");

    let recorded = fixture.recorded();
    assert_eq!(
        recorded.len(),
        6,
        "two creates, an update, a rewrite, a delete"
    );
    let WorkspaceDelta::Created { workspace } = &recorded[0] else {
        panic!("a create publishes a full row, so a subscriber needs no second query");
    };
    assert_eq!(workspace.id.as_str(), target.id);
    assert_eq!(workspace.worker_fp.as_str(), WORKER_FP);
    assert_eq!(workspace.name, "ws-/srv/two");
    assert_eq!(workspace.folder_path, "/srv/two");
    assert_eq!(workspace.session_ids, Vec::new());
    assert_eq!(workspace.version, 0, "a fresh row starts at version 0");
    match &recorded[2] {
        WorkspaceDelta::Updated { workspace } => {
            assert_eq!(workspace.id.as_str(), target.id);
            assert_eq!(workspace.color.as_deref(), Some("amber"));
            assert_eq!(
                workspace.version, 1,
                "the delta carries the version the write produced"
            );
        }
        other => panic!("expected an update, got {other:?}"),
    }
    match &recorded[3] {
        WorkspaceDelta::SessionsSet {
            id,
            session_ids,
            version,
        } => {
            assert_eq!(id.as_str(), target.id);
            assert_eq!(
                session_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                [SESSION_A]
            );
            assert_eq!(*version, 2);
        }
        other => panic!("expected a membership, got {other:?}"),
    }
    assert_eq!(
        recorded[5],
        WorkspaceDelta::Deleted {
            id: workspace_id(&source.id),
        },
        "the workspace the rewrite emptied is announced as deleted, not as emptied"
    );
    assert!(
        matches!(&recorded[4], WorkspaceDelta::Deleted { id } if id.as_str() == source.id),
        "the source the rewrite emptied is announced as deleted, after the target's own \
         membership move: a subscriber applies them in the order they happened"
    );
}

/// The delta and the list are the same answer. A client that listens and a
/// client that re-fetches must not be able to end up with different rows.
#[tokio::test]
async fn a_delta_and_the_list_agree_field_for_field() {
    let fixture = WorkspacesFixture::new("same-source").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    let created = create(&fixture, "/srv/one", &[SESSION_A]).await;

    let listed = handle_workspaces_list(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesListRequest::default(),
    )
    .await
    .expect("the list")
    .body
    .workspaces;
    assert_eq!(listed.len(), 1);
    assert_eq!(
        listed[0], created,
        "the row the response carried is the row the list carries"
    );
    let WorkspaceDelta::Created { workspace } = &fixture.recorded()[0] else {
        panic!("a create publishes a full row");
    };
    assert_eq!(workspace.id.as_str(), created.id);
    assert_eq!(workspace.name, created.name);
    assert_eq!(workspace.folder_path, created.folder_path);
    assert_eq!(workspace.position, i64::from(created.position));
    assert_eq!(
        workspace.version,
        i64::try_from(created.version).expect("a version that fits i64")
    );
    assert_eq!(
        workspace
            .session_ids
            .iter()
            .map(|id| id.as_str().to_owned())
            .collect::<Vec<_>>(),
        created.session_ids,
        "the broadcast membership is the membership the list answers with"
    );
}

/// A write nobody could commit publishes nothing: the dedupe that answers with an
/// existing row, and a version claim that lost.
#[tokio::test]
async fn a_write_that_changed_nothing_publishes_nothing() {
    let fixture = WorkspacesFixture::new("silent").await;
    let first = create(&fixture, "/srv/one", &[]).await;
    let again = handle_workspaces_create(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesCreateRequest {
            worker_fp: WORKER_FP.to_owned(),
            name: "another name".to_owned(),
            folder_path: "/srv/one".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("the existing row");
    assert_eq!(
        again.body.workspace.into_option().expect("a row").id,
        first.id
    );
    assert_eq!(
        fixture.recorded().len(),
        1,
        "the dedupe wrote nothing, so it announced nothing"
    );

    let refused = handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: first.id.clone(),
            if_version: 9,
            name: Some("renamed".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect_err("a stale version");
    assert!(
        refused
            .message
            .unwrap_or_default()
            .contains("version mismatch")
    );
    assert_eq!(
        fixture.recorded().len(),
        1,
        "a refused write announces nothing"
    );
}

/// A mutation is refused while an exclusive drain holds the write gate, which is
/// the fence a keeper update relies on.
#[tokio::test]
async fn a_mutation_is_refused_while_a_keeper_update_drains() {
    let fixture = WorkspacesFixture::new("drain").await;
    let drain = fixture
        .core
        .services
        .write_gate()
        .acquire_exclusive()
        .expect("the drain a keeper update takes");
    let refused = handle_workspaces_create(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesCreateRequest {
            worker_fp: WORKER_FP.to_owned(),
            name: "ws".to_owned(),
            folder_path: "/srv/one".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("the drain is exclusive");
    assert_eq!(refused.code, connectrpc::ErrorCode::Unavailable);
    assert!(fixture.recorded().is_empty());
    drop(drain);

    let created = create(&fixture, "/srv/one", &[]).await;
    assert_eq!(
        created.name, "ws",
        "the write lands once the drain is released"
    );
}
