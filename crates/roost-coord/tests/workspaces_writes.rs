// The four writes' own rules: the dedupe that keeps one folder one workspace,
// the version each write is conditioned on, and the principal every one of them
// requires. The cascade that a delete and a rewrite share is in
// `workspaces_tree.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workspaces_support;

use connectrpc::ErrorCode;
use roost_coord::sessions::rpc_workspaces::{
    handle_workspaces_create, handle_workspaces_delete, handle_workspaces_set_sessions,
    handle_workspaces_update, handle_workspaces_list,
};

use workspaces_support::{SESSION_A, WORKER_FP, WorkspacesFixture, device_caller, machine_caller};

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

/// A second create for a folder that already has a workspace returns that row.
#[tokio::test]
async fn a_folder_that_already_has_a_workspace_is_not_created_twice() {
    let fixture = WorkspacesFixture::new("dedupe").await;
    let first = create(&fixture, "/srv/one", &[]).await;
    let again = handle_workspaces_create(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesCreateRequest {
            worker_fp: WORKER_FP.to_owned(),
            name: "a different name".to_owned(),
            folder_path: "/srv/one".to_owned(),
            attach_session_ids: Vec::new(),
            ..Default::default()
        },
    )
    .await
    .expect("the existing row")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");
    assert_eq!(again.id, first);
    assert_eq!(again.name, "ws-/srv/one", "the existing row's name is not rewritten");
    assert_eq!(fixture.scalar_i64("SELECT COUNT(*) FROM workspaces").await, 1);
}

/// A session's realpath is the folder its pane opened, and it outranks the path
/// the request named.
#[tokio::test]
async fn an_attached_sessions_folder_outranks_the_requested_one() {
    let fixture = WorkspacesFixture::new("cwd").await;
    fixture.enroll_session(SESSION_A, "/srv/real").await;
    let created = handle_workspaces_create(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesCreateRequest {
            worker_fp: WORKER_FP.to_owned(),
            name: "ws".to_owned(),
            folder_path: "/srv/typed".to_owned(),
            attach_session_ids: vec![SESSION_A.to_owned()],
            ..Default::default()
        },
    )
    .await
    .expect("a created workspace")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");
    assert_eq!(created.folder_path, "/srv/real");
    assert_eq!(created.session_ids, vec![SESSION_A.to_owned()]);
    assert_eq!(
        fixture.session_workspace_id(SESSION_A).await,
        created.id,
        "both representations of membership name the same workspace"
    );
}

/// A stale `if_version` is a precondition failure, on every write that carries
/// one, and it changes nothing.
#[tokio::test]
async fn a_stale_version_is_refused_and_writes_nothing() {
    let fixture = WorkspacesFixture::new("cas").await;
    fixture.enroll_session(SESSION_A, "/srv/one").await;
    let workspace = create(&fixture, "/srv/one", &[SESSION_A]).await;

    let renamed = handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: workspace.clone(),
            if_version: 7,
            name: Some("renamed".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect_err("a stale version");
    assert_eq!(renamed.code, ErrorCode::FailedPrecondition);

    let deleted = handle_workspaces_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesDeleteRequest {
            id: workspace.clone(),
            if_version: 7,
        },
    )
    .await
    .expect_err("a stale version");
    assert_eq!(deleted.code, ErrorCode::FailedPrecondition);

    let moved = handle_workspaces_set_sessions(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesSetSessionsRequest {
            id: workspace.clone(),
            if_version: 7,
            session_ids: Vec::new(),
        },
    )
    .await
    .expect_err("a stale version");
    assert_eq!(moved.code, ErrorCode::FailedPrecondition);

    assert_eq!(fixture.junction_rows(&workspace).await, 1, "no membership moved");
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT version FROM workspaces WHERE id = '{workspace}'"
            ))
            .await,
        0
    );
}

/// An update that names no field still spends the claim, so a client that
/// re-reads sees that its write happened.
#[tokio::test]
async fn an_update_with_no_fields_still_bumps_the_version() {
    let fixture = WorkspacesFixture::new("no-fields").await;
    let workspace = create(&fixture, "/srv/one", &[]).await;
    let updated = handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: workspace.clone(),
            if_version: 0,
            ..Default::default()
        },
    )
    .await
    .expect("an update that named nothing")
    .body
    .workspace
    .into_option()
    .expect("a workspace in the response");
    assert_eq!(updated.version, 1);
    let repeat = handle_workspaces_update(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkspacesUpdateRequest {
            id: workspace,
            if_version: 0,
            ..Default::default()
        },
    )
    .await
    .expect_err("the claim was spent");
    assert_eq!(repeat.code, ErrorCode::FailedPrecondition);
}

/// The five methods are account-device methods, and a machine is not one.
#[tokio::test]
async fn a_machine_may_not_manage_workspaces() {
    let fixture = WorkspacesFixture::new("auth").await;
    let refused = handle_workspaces_list(
        &fixture.core,
        &machine_caller(),
        roost_proto::WorkspacesListRequest::default(),
    )
    .await
    .expect_err("a machine is not a browser");
    assert_eq!(refused.code, ErrorCode::Unauthenticated);
}
