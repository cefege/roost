// The five Connect methods on their happy path, over a real database with NO
// terminal hub in the process.
//
// The fixture installs `NoTerminalSeams`, which is the whole point: if a handler
// reached for a terminal type instead of a seam, this file would not compile,
// and if it needed a live byte hub to answer, it would not run. The refusals are
// in `workers_refusals.rs`; this file is about the rows, the presence and the
// routable set the five methods produce.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workers_support;

use roost_coord::workers::rpc::{
    handle_workers_delete, handle_workers_heartbeat, handle_workers_list, handle_workers_register,
    handle_workers_rename,
};
use roost_proto::buffa::MessageField;
use roost_protocol::wire::WorkerPresenceEvent;

use workers_support::{
    OTHER_WORKER_FP, SESSION_ID, WORKER_FP, WorkersFixture, device_caller, worker_caller,
};

#[tokio::test]
async fn the_five_methods_round_trip_and_the_list_projects_the_row() {
    let fixture = WorkersFixture::new("round-trip").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    fixture
        .enroll_worker(OTHER_WORKER_FP, "spare-box", 2_000)
        .await;
    let socket = std::sync::Arc::new(workers_support::RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);

    // Register: the row is rewritten from the worker's own statement.
    let registered = handle_workers_register(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersRegisterRequest {
            label: Some("build-box".to_owned()),
            os: Some("linux".to_owned()),
            git_sha: Some("abc123".to_owned()),
            reachable_addr: Some("build-box.tail.ts.net".to_owned()),
            host_identity: MessageField::none(),
            ..Default::default()
        },
    )
    .await
    .expect("a registered worker")
    .body
    .worker
    .into_option()
    .expect("a worker");
    assert_eq!(registered.fp, WORKER_FP);
    assert_eq!(registered.label, "build-box");
    assert_eq!(registered.os, "linux");
    assert_eq!(registered.git_sha.as_deref(), Some("abc123"));
    assert!(registered.last_seen_ms > 0, "the beat is stamped");
    assert_eq!(
        registered.registered_at_ms, 1_000,
        "enrollment time is not a beat"
    );

    // Register again: idempotent for one fingerprint, and an absent claim keeps
    // the stored value rather than clearing it.
    let repeated = handle_workers_register(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersRegisterRequest::default(),
    )
    .await
    .expect("a repeated register is the same answer")
    .body
    .worker
    .into_option()
    .expect("a worker");
    assert_eq!(repeated.fp, registered.fp);
    assert_eq!(repeated.label, registered.label);
    assert_eq!(repeated.git_sha, registered.git_sha);
    assert_eq!(
        repeated.reachable_addr, registered.reachable_addr,
        "an absent claim keeps the prior value"
    );
    assert_eq!(repeated.registered_at_ms, registered.registered_at_ms);
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workers WHERE fp = '{WORKER_FP}'"
            ))
            .await,
        1,
        "one fingerprint is one row"
    );

    // Heartbeat: a beat that re-asserts nothing new is still a beat.
    let beat = handle_workers_heartbeat(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersHeartbeatRequest {
            git_sha: Some("abc123".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("a heartbeat");
    assert_eq!(beat.body, roost_proto::WorkersHeartbeatResponse::default());

    // Rename: the operator-facing label moves.
    let renamed = handle_workers_rename(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersRenameRequest {
            fp: WORKER_FP.to_owned(),
            label: "renamed-box".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a rename");
    assert_eq!(
        renamed.body.worker.into_option().expect("a worker").label,
        "renamed-box"
    );

    // List: the rows the v2 renderer expects, plus the routable set.
    let listed = handle_workers_list(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersListRequest::default(),
    )
    .await
    .expect("a worker list")
    .body;
    assert_eq!(listed.workers.len(), 2, "both machines are live");
    assert_eq!(listed.workers[0].fp, WORKER_FP, "enrollment order");
    assert_eq!(listed.workers[0].label, "renamed-box");
    assert_eq!(listed.workers[0].os, "linux");
    assert_eq!(listed.workers[0].git_sha.as_deref(), Some("abc123"));
    assert_eq!(listed.workers[1].fp, OTHER_WORKER_FP);
    assert_eq!(
        listed.routable_fps,
        vec![WORKER_FP.to_owned()],
        "only the machine with a live socket is routable"
    );

    // Delete: the row is tombstoned, its session rows survive, and the
    // credential can no longer be used.
    fixture.enroll_session(WORKER_FP, SESSION_ID).await;
    let deleted = handle_workers_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a delete");
    assert!(deleted.body.ok);
    assert!(
        fixture
            .scalar_i64(&format!(
                "SELECT deleted_at_ms FROM workers WHERE fp = '{WORKER_FP}'"
            ))
            .await
            > 0,
        "the row is tombstoned, not removed"
    );
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM sessions WHERE id = '{SESSION_ID}'"
            ))
            .await,
        1,
        "a session row is history, and a delete does not erase it"
    );

    // And the tombstoned machine is gone from the fleet view.
    let after = handle_workers_list(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersListRequest::default(),
    )
    .await
    .expect("a worker list")
    .body;
    assert_eq!(after.workers.len(), 1);
    assert_eq!(after.workers[0].fp, OTHER_WORKER_FP);
    assert!(after.routable_fps.is_empty());
}

/// The presence bus sees every transition, and the routable set follows the
/// sockets rather than the heartbeat.
#[tokio::test]
async fn presence_carries_every_transition_and_routable_follows_the_socket() {
    let fixture = WorkersFixture::new("presence").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorder = std::sync::Arc::clone(&seen);
    let _subscription = fixture
        .core
        .services
        .buses
        .presence_bus
        .subscribe(move |event| {
            recorder
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(event.clone());
        });
    let routable = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let routable_recorder = std::sync::Arc::clone(&routable);
    let _routable_subscription =
        fixture
            .core
            .services
            .buses
            .worker_routable_bus
            .subscribe(move |set| {
                routable_recorder
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .push(set.fps.clone());
            });

    let socket = std::sync::Arc::new(workers_support::RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);
    handle_workers_register(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersRegisterRequest {
            label: Some("build-box".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("a register");
    handle_workers_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect("a delete");

    let events = seen
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let kinds: Vec<&'static str> = events
        .iter()
        .map(|event| match event {
            WorkerPresenceEvent::Registered { .. } => "registered",
            WorkerPresenceEvent::Heartbeat { .. } => "heartbeat",
            WorkerPresenceEvent::Removed { .. } => "removed",
        })
        .collect();
    assert_eq!(kinds, vec!["registered", "removed"]);
    assert!(
        matches!(&events[0], WorkerPresenceEvent::Registered { worker } if worker.label == "build-box"),
        "a registration publishes the whole record, got {:?}",
        events[0]
    );
    assert!(
        matches!(&events[1], WorkerPresenceEvent::Removed { fp } if fp.as_str() == WORKER_FP),
        "a deletion names the machine, got {:?}",
        events[1]
    );

    let sets = routable
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .clone();
    let rendered: Vec<Vec<String>> = sets
        .iter()
        .map(|fps| fps.iter().map(ToString::to_string).collect())
        .collect();
    assert!(
        rendered.contains(&vec![WORKER_FP.to_owned()]),
        "the generation becoming routable published the set, got {rendered:?}"
    );
    assert_eq!(
        rendered.last(),
        Some(&Vec::new()),
        "the delete republished the set without the machine"
    );
}
