//! Who a Sync socket belongs to: a worker's read-only socket narrowed to its
//! own resources in its index, its backfill and its live feed, the install-wide
//! audit stream only for install-wide viewers, and every open socket of a
//! revoked device or a deleted worker closed `4001 revoked`.
//!
//! Ports `apps/coord/tests/sync/sync-worker-socket-scope.test.ts`, the worker
//! and v1 cases of `sync-audit-subscription.test.ts`, and the
//! `closeForFingerprint` hooks of `apps/coord/src/main.ts` (device revocation)
//! and `workers/handlers-workers.ts` (worker delete).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod sync_seed_support;
mod sync_ws_socket_support;
mod ws_client_support;
mod ws_credential_support;

use std::collections::BTreeSet;

use roost_coord::auth::principal::Principal;
use roost_coord::auth::rpc_devices::handle_devices_revoke;
use roost_coord::coord_core::{Caller, CoordCore, ListenerTrust};
use roost_coord::events::bus_messages::{AuditRow, SessionTitleUpdate};
use roost_coord::sync_ws::resource_index::load_sync_resource_index;
use roost_coord::workers::rpc::handle_workers_delete;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::{
    DevicesRevokeRequest, SyncDomain, SyncDomainSubscriptionCommand, WorkersDeleteRequest,
};

use sync_seed_support::{
    closed_message, closed_of, dashboard, enroll_worker, exec, insert_closed_row, insert_session,
    insert_worker,
};
use sync_ws_socket_support::{
    EXPECT, QUIET, SyncFixture, domain_ready, generation_of, next_firehose, read_subscribed,
    send_client_frame,
};
use ws_client_support::close_code;

const WORKER_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SESSION_A: &str = "10000000-0000-4000-8000-000000000001";
const SESSION_B: &str = "20000000-0000-4000-8000-000000000002";
const WORKSPACE_A: &str = "30000000-0000-4000-8000-000000000001";
const WORKSPACE_B: &str = "40000000-0000-4000-8000-000000000002";

/// Two workers, one session and one workspace each, and the durable rows
/// closed A, closed B, closed A. Returns worker A's fingerprint and token and
/// the three row ids.
async fn two_worker_install(fixture: &SyncFixture) -> (String, String, Vec<u64>) {
    let (worker_a, token) = enroll_worker(fixture, 71).await;
    insert_worker(fixture, WORKER_B).await;
    insert_session(fixture, SESSION_A, &worker_a).await;
    insert_session(fixture, SESSION_B, WORKER_B).await;
    let dashboard = dashboard(fixture);
    for (id, worker) in [(WORKSPACE_A, worker_a.as_str()), (WORKSPACE_B, WORKER_B)] {
        exec(
            fixture,
            &format!(
                "INSERT INTO workspaces (id, worker_fp, name, created_at_ms, updated_at_ms, \
                 dashboard_id) VALUES ('{id}', '{worker}', 'w', 1000, 1000, '{dashboard}')"
            ),
        )
        .await;
    }
    let mut ids = Vec::new();
    for (session_id, ts) in [(SESSION_A, 1), (SESSION_B, 2), (SESSION_A, 3)] {
        ids.push(insert_closed_row(fixture, session_id, ts).await);
    }
    (worker_a, token, ids)
}

// v2 "a worker index holds only its own resources while a browser index spans
// the install".
#[tokio::test]
async fn a_worker_index_holds_only_its_own_resources() {
    let fixture = SyncFixture::start("scope-index").await;
    let (worker_a, _token, _ids) = two_worker_install(&fixture).await;
    let db = &fixture.services.db;
    let worker = load_sync_resource_index(db, Some(&worker_a)).await.unwrap();
    assert_eq!(worker.owner_worker_fp.as_deref(), Some(worker_a.as_str()));
    let fps: Vec<&str> = worker.worker_fps.iter().map(|fp| fp.as_str()).collect();
    assert_eq!(fps, vec![worker_a.as_str()]);
    assert_eq!(worker.session_ids, BTreeSet::from([SESSION_A.to_owned()]));
    assert_eq!(
        worker.workspace_ids,
        BTreeSet::from([WORKSPACE_A.to_owned()])
    );

    let browser = load_sync_resource_index(db, None).await.unwrap();
    assert_eq!(browser.owner_worker_fp, None);
    assert_eq!(browser.worker_fps.len(), 2);
    assert_eq!(
        browser.session_ids,
        BTreeSet::from([SESSION_A.to_owned(), SESSION_B.to_owned()])
    );
    assert_eq!(browser.workspace_ids.len(), 2);
}

// v2 "a worker feed excludes another worker's durable and live session
// frames": the backfill, the titles and the live session events all stop at
// the worker's own sessions, and a read-only socket gets no UI seed.
#[tokio::test]
async fn a_worker_socket_carries_only_its_own_durable_and_live_session_frames() {
    let fixture = SyncFixture::start("scope-feed").await;
    let (_worker_a, token, ids) = two_worker_install(&fixture).await;
    let mut socket = fixture
        .dial_sync(&format!("since={}", ids[0]), &token)
        .await
        .socket();
    let seed = next_firehose(&mut socket, EXPECT).await.expect("the seed");
    assert!(matches!(seed.frame, Some(Frame::WorkerRoutable(_))));
    let replayed = next_firehose(&mut socket, EXPECT).await.expect("A's row");
    assert_eq!(closed_of(&replayed), Some((ids[2], SESSION_A.to_owned())));

    let buses = &fixture.services.buses;
    for session_id in [SESSION_A, SESSION_B] {
        buses.title_bus.publish(SessionTitleUpdate {
            session_id: session_id.to_owned(),
            title: "t".to_owned(),
        });
    }
    buses
        .session_bus
        .publish(closed_message(SESSION_A, ids[2] + 1));
    buses
        .session_bus
        .publish(closed_message(SESSION_B, ids[2] + 2));
    let title = next_firehose(&mut socket, EXPECT).await.expect("A's title");
    let Some(Frame::TerminalTitle(title)) = title.frame else {
        panic!("expected A's title");
    };
    assert_eq!(title.session_id, SESSION_A);
    let live = next_firehose(&mut socket, EXPECT)
        .await
        .expect("A's live close");
    assert_eq!(closed_of(&live), Some((ids[2] + 1, SESSION_A.to_owned())));
    assert!(
        next_firehose(&mut socket, QUIET).await.is_none(),
        "nothing of B's"
    );
}

fn audit_row() -> AuditRow {
    AuditRow {
        id: 1,
        ts: 1,
        caller_fp: None,
        caller_label: None,
        method: "Ping".to_owned(),
        path: "/ping".to_owned(),
        status: 200,
        trace_id: None,
    }
}

// v2 sync-audit-subscription.test.ts "worker-scoped Sync v2 feeds cannot
// subscribe to install-wide audits" and "Sync v1 retains eager install-wide
// audit delivery".
#[tokio::test]
async fn audit_rows_reach_install_wide_viewers_only() {
    let fixture = SyncFixture::start("scope-audit").await;
    let (_worker_a, worker_token, _ids) = two_worker_install(&fixture).await;
    let (_browser, browser_token) = fixture.enroll_browser(72).await;
    let mut worker = fixture
        .dial_sync("flow=1&sync_v=2", &worker_token)
        .await
        .socket();
    let subscribed = read_subscribed(&mut worker).await;
    let audit = generation_of(&subscribed, SyncDomain::Audit);
    let subscribe = Command::DomainSubscribe(Box::new(SyncDomainSubscriptionCommand {
        domain: SyncDomain::Audit.into(),
        generation: audit,
        ..SyncDomainSubscriptionCommand::default()
    }));
    send_client_frame(&mut worker, &subscribed.socket_id, None, Some(subscribe)).await;
    send_client_frame(
        &mut worker,
        &subscribed.socket_id,
        None,
        Some(domain_ready(SyncDomain::Audit, audit)),
    )
    .await;
    let mut legacy = fixture.dial_sync("", &browser_token).await.socket();
    let seed = next_firehose(&mut legacy, EXPECT)
        .await
        .expect("the v1 seed");
    assert!(matches!(seed.frame, Some(Frame::WorkerRoutable(_))));

    fixture.services.buses.audit_bus.publish(audit_row());
    let row = next_firehose(&mut legacy, EXPECT)
        .await
        .expect("v1 audit is eager");
    assert!(matches!(row.frame, Some(Frame::AuditRow(_))));
    assert!(
        next_firehose(&mut worker, QUIET).await.is_none(),
        "a worker is not an audit viewer"
    );
}

fn on_host_browser(fingerprint: &str, account_id: &str) -> Caller {
    Caller {
        principal: Principal::AccountDevice {
            fingerprint: fingerprint.to_owned(),
            label: "operator".to_owned(),
            account_id: account_id.to_owned(),
        },
        tab_id: None,
        remote_address: Some("127.0.0.1".to_owned()),
        on_host: true,
        listener_trust: ListenerTrust::DirectLoopback,
    }
}

// main.ts `onKeyRevoked` -> sync-ws-handler.ts `closeForFingerprint`: revoking
// a device closes every Sync socket it holds, v1 and v2, with 4001 at once --
// and nobody else's.
#[tokio::test]
async fn a_revoked_device_loses_every_open_sync_socket_with_4001() {
    let fixture = SyncFixture::start("scope-revoke").await;
    let (victim, token) = fixture.enroll_browser(73).await;
    let (_bystander, bystander_token) = fixture.enroll_browser(74).await;
    let mut legacy = fixture.dial_sync("flow=1", &token).await.socket();
    let mut v2 = fixture
        .dial_sync("flow=1&sync_v=2&tab=t1", &token)
        .await
        .socket();
    let mut other = fixture
        .dial_sync("flow=1&sync_v=2&tab=t2", &bystander_token)
        .await
        .socket();
    next_firehose(&mut legacy, EXPECT)
        .await
        .expect("the v1 seed");
    read_subscribed(&mut v2).await;
    let bystander = read_subscribed(&mut other).await;

    let core = CoordCore::new(fixture.services.clone());
    let account_id = fixture
        .services
        .boot
        .tenant
        .as_ref()
        .unwrap()
        .account_id
        .clone();
    let request = DevicesRevokeRequest {
        fingerprint: victim,
        ..DevicesRevokeRequest::default()
    };
    handle_devices_revoke(&core, &on_host_browser("operator", &account_id), request)
        .await
        .expect("the revoke commits");
    assert_eq!(close_code(&mut legacy, EXPECT).await, Some(Some(4001)));
    assert_eq!(close_code(&mut v2, EXPECT).await, Some(Some(4001)));

    fixture.publish_task("still-live", 4);
    let tasks = generation_of(&bystander, SyncDomain::Tasks);
    send_client_frame(
        &mut other,
        &bystander.socket_id,
        None,
        Some(domain_ready(SyncDomain::Tasks, tasks)),
    )
    .await;
    let frame = next_firehose(&mut other, EXPECT)
        .await
        .expect("the bystander is untouched");
    assert!(matches!(frame.frame, Some(Frame::TaskDelta(_))));
}

// handlers-workers.ts `onWorkerDeletedSocketClose`: deleting a worker closes
// its own read-only Sync socket with 4001.
#[tokio::test]
async fn a_deleted_worker_loses_its_sync_socket_with_4001() {
    let fixture = SyncFixture::start("scope-delete").await;
    let (worker_a, token, _ids) = two_worker_install(&fixture).await;
    let mut socket = fixture.dial_sync("", &token).await.socket();
    next_firehose(&mut socket, EXPECT).await.expect("the seed");

    let core = CoordCore::new(fixture.services.clone());
    let account_id = fixture
        .services
        .boot
        .tenant
        .as_ref()
        .unwrap()
        .account_id
        .clone();
    let request = WorkersDeleteRequest {
        fp: worker_a,
        ..WorkersDeleteRequest::default()
    };
    handle_workers_delete(&core, &on_host_browser("operator", &account_id), request)
        .await
        .expect("the delete commits");
    assert_eq!(close_code(&mut socket, EXPECT).await, Some(Some(4001)));
}
