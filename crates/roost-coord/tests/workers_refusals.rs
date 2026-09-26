// Every refusal the five worker methods can answer, and the fact each message
// has to carry.
//
// A refusal is the only thing a caller gets when something is wrong, so what it
// says is the interface: "worker not registered" on its own sends an operator
// looking for a bootstrap token when the real answer is that they deleted the
// machine an hour ago. Each test here asserts the code AND the fact.

// `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
// test is its own crate rather than a module of one, so the exemption has to be
// stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod workers_support;

use connectrpc::ErrorCode;
use roost_coord::workers::rpc::{
    handle_workers_delete, handle_workers_heartbeat, handle_workers_list, handle_workers_register,
    handle_workers_rename,
};

use workers_support::{OTHER_WORKER_FP, WORKER_FP, WorkersFixture, device_caller, worker_caller};

fn message_of(error: &connectrpc::ConnectError) -> String {
    error.message.clone().unwrap_or_default()
}

/// A machine that is not there is NotFound for both operator methods, and the
/// refusal does not say "tombstoned" for a row that never existed.
#[tokio::test]
async fn an_absent_machine_is_not_found() {
    let fixture = WorkersFixture::new("absent").await;
    let renamed = handle_workers_rename(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersRenameRequest {
            fp: WORKER_FP.to_owned(),
            label: "ghost".to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("there is no such machine");
    assert_eq!(renamed.code, ErrorCode::NotFound);
    assert_eq!(message_of(&renamed), "worker not found");

    let deleted = handle_workers_delete(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersDeleteRequest {
            fp: WORKER_FP.to_owned(),
            ..Default::default()
        },
    )
    .await
    .expect_err("there is no such machine");
    assert_eq!(deleted.code, ErrorCode::NotFound);
}

/// A worker principal may not read the fleet view, and an account device may not
/// register a machine: the wrong kind is refused before any row moves.
#[tokio::test]
async fn a_principal_of_the_wrong_kind_is_refused() {
    let fixture = WorkersFixture::new("principal").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;

    let as_device = handle_workers_register(
        &fixture.core,
        &device_caller(),
        roost_proto::WorkersRegisterRequest::default(),
    )
    .await
    .expect_err("a browser cannot register a machine");
    assert_eq!(as_device.code, ErrorCode::Unauthenticated);

    let as_worker = handle_workers_list(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersListRequest::default(),
    )
    .await
    .expect_err("a machine cannot read the fleet view");
    assert_eq!(as_worker.code, ErrorCode::Unauthenticated);

    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workers WHERE fp = '{WORKER_FP}'"
            ))
            .await,
        1,
        "a refused register writes nothing"
    );
}

/// A platform this product does not support is refused on the way in, before the
/// row is rewritten.
#[tokio::test]
async fn an_unsupported_platform_is_refused() {
    let fixture = WorkersFixture::new("platform").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;

    let refusal = handle_workers_register(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersRegisterRequest {
            os: Some("plan9".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect_err("plan9 is not a platform this product supports");
    assert_eq!(refusal.code, ErrorCode::InvalidArgument);
    assert!(
        message_of(&refusal).contains("unsupported worker os"),
        "the refusal names the rule, got: {refusal:?}"
    );
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT last_seen_ms FROM workers WHERE fp = '{WORKER_FP}'"
            ))
            .await,
        1_000,
        "a refused register does not stamp the row as heard from"
    );
}

/// A tombstoned worker is refused by name: the refusal says the machine was
/// deleted and when, which is the only fact on the row that explains it.
#[tokio::test]
async fn a_heartbeat_from_a_tombstoned_worker_names_the_tombstone() {
    let fixture = WorkersFixture::new("tombstone").await;
    fixture.enroll_worker(WORKER_FP, "build-box", 1_000).await;
    let socket = std::sync::Arc::new(workers_support::RecordingSocket::new());
    fixture.connect_worker(WORKER_FP, "generation-a", &socket);
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

    let refusal = handle_workers_heartbeat(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersHeartbeatRequest::default(),
    )
    .await
    .expect_err("a deleted worker does not get to beat");
    assert_eq!(refusal.code, ErrorCode::Unauthenticated);
    let message = message_of(&refusal);
    assert!(
        message.contains("was deleted at"),
        "the refusal names the tombstone, got: {message}"
    );
    assert!(
        message.contains(WORKER_FP),
        "and names the machine, got: {message}"
    );

    // A worker that was never enrolled still gets the bootstrap-token answer, so
    // the two states stay distinguishable.
    let never = handle_workers_heartbeat(
        &fixture.core,
        &worker_caller(OTHER_WORKER_FP),
        roost_proto::WorkersHeartbeatRequest::default(),
    )
    .await
    .expect_err("an unenrolled worker does not get to beat");
    assert!(
        message_of(&never).contains("redeem bootstrap token"),
        "got: {never:?}"
    );
}

/// A register from a machine that never redeemed a token is refused, and writes
/// nothing: this is the state the bootstrap ceremony exists to prevent.
#[tokio::test]
async fn a_register_from_an_unenrolled_worker_is_refused() {
    let fixture = WorkersFixture::new("unenrolled").await;
    let refusal = handle_workers_register(
        &fixture.core,
        &worker_caller(WORKER_FP),
        roost_proto::WorkersRegisterRequest {
            label: Some("build-box".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect_err("a machine that never redeemed a token has no row");
    assert_eq!(refusal.code, ErrorCode::Unauthenticated);
    assert!(
        message_of(&refusal).contains("redeem bootstrap token"),
        "got: {refusal:?}"
    );
    assert_eq!(
        fixture
            .scalar_i64(&format!(
                "SELECT COUNT(*) FROM workers WHERE fp = '{WORKER_FP}'"
            ))
            .await,
        0,
        "a refused register writes nothing"
    );
}
