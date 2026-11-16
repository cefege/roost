//! The registry arms, decoded from the frames `roost-coord`'s feed builds and
//! applied through `ClientCore::handle`: workspaces and
//! worker presence. The routable set and audit rows are `sync_decode_routable`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::SyncDomain;
use roost_client_core::event::ClientEvent;
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::__buffa::oneof::worker_presence_proto::Kind as PresenceKind;
use roost_proto::__buffa::oneof::workspace_delta_proto::Kind as WorkspaceKind;
use roost_proto::{
    Worker, WorkerHeartbeat, WorkerPresenceProto, Workspace, WorkspaceDeltaProto,
    WorkspaceSessionsSet,
};

use sync_decode_support::{SESSION, WORKER_FP, acked, application, deliver, ready_core};

const WORKSPACE: &str = "00000000-0000-4000-8000-0000000000a1";

fn workspace_arm(kind: WorkspaceKind) -> Frame {
    Frame::WorkspaceDelta(Box::new(WorkspaceDeltaProto {
        kind: Some(kind),
        ..WorkspaceDeltaProto::default()
    }))
}

fn workspace_row(name: &str) -> Workspace {
    Workspace {
        id: WORKSPACE.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        name: name.to_owned(),
        folder_path: "/srv/app".to_owned(),
        version: 1,
        created_at_ms: 10,
        updated_at_ms: 20,
        ..Workspace::default()
    }
}

#[test]
fn a_workspace_is_created_given_sessions_and_deleted() {
    let (mut core, generation) = ready_core();
    let created = workspace_arm(WorkspaceKind::Created(Box::new(workspace_row("Server"))));
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workspaces, 1, created),
    );
    assert_eq!(acked(&effects), vec![1]);
    assert_eq!(core.store().workspaces[WORKSPACE].name, "Server");

    let set = workspace_arm(WorkspaceKind::SessionsSet(Box::new(WorkspaceSessionsSet {
        workspace_id: WORKSPACE.to_owned(),
        session_ids: vec![SESSION.to_owned()],
        version: 2,
        ..WorkspaceSessionsSet::default()
    })));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workspaces, 2, set),
    );
    let held = &core.store().workspaces[WORKSPACE];
    assert_eq!((held.session_ids.len(), held.version), (1, 2));

    let deleted = workspace_arm(WorkspaceKind::DeletedId(WORKSPACE.to_owned()));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workspaces, 3, deleted),
    );
    assert!(core.store().workspaces.is_empty());
}

#[test]
fn a_registry_frame_waits_for_its_own_domain_not_any_ready_domain() {
    // A frame is gated on its own domain: a workspaces
    // delta is not admissible because the workers domain is ready.
    let (mut core, generation) = ready_core();
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: roost_client_core::SyncFrame::DomainReset {
            domain: SyncDomain::Workspaces,
            generation: 4,
            reason: "test".to_owned(),
            subscribed: true,
        },
    });
    let created = workspace_arm(WorkspaceKind::Created(Box::new(workspace_row("Early"))));
    let effects = deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workspaces, 5, created),
    );
    assert!(acked(&effects).is_empty());
    assert!(core.store().workspaces.is_empty());
}

fn presence_arm(kind: PresenceKind) -> Frame {
    Frame::WorkerPresence(Box::new(WorkerPresenceProto {
        kind: Some(kind),
        ..WorkerPresenceProto::default()
    }))
}

#[test]
fn a_worker_registers_heartbeats_and_is_removed() {
    let (mut core, generation) = ready_core();
    let registered = presence_arm(PresenceKind::Registered(Box::new(Worker {
        fp: WORKER_FP.to_owned(),
        label: "build-box".to_owned(),
        os: "linux".to_owned(),
        registered_at_ms: 1,
        last_seen_ms: 2,
        ..Worker::default()
    })));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 1, registered),
    );
    assert_eq!(core.store().workers[WORKER_FP].last_seen_ms, 2);

    let heartbeat = presence_arm(PresenceKind::Heartbeat(Box::new(WorkerHeartbeat {
        worker_fp: WORKER_FP.to_owned(),
        last_seen_ms: 90,
        ..WorkerHeartbeat::default()
    })));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 2, heartbeat),
    );
    let worker = &core.store().workers[WORKER_FP];
    assert_eq!(
        (worker.last_seen_ms, worker.label.as_str()),
        (90, "build-box")
    );

    let removed = presence_arm(PresenceKind::RemovedFp(WORKER_FP.to_owned()));
    deliver(
        &mut core,
        generation,
        &application(SyncDomain::Workers, 3, removed),
    );
    assert!(core.store().workers.is_empty());
}
