//! The local-terminal grant arms of the downstream dispatch: a worker with a
//! local door acknowledges an install with its grant id, answers a refused
//! install with the store's reason, and routes a revocation to the door; a
//! worker without one answers v2's absent-owner refusal. Ports the
//! `localTerminalGrant`/`localTerminalGrantRevoke` cases of
//! `apps/worker/src/transport/coord-link-downstream.ts:269-300`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode};
use roost_proto::{DLocalTerminalGrant, DLocalTerminalGrantRevoke};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream as Down, CoordWorkerUpstream as Up};
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::channel;

const EPOCH: &str = "process-epoch-9";

fn dispatch(fakes: Option<&Fakes>, frame: Down) -> Vec<Up> {
    let (uplink, _receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, EPOCH, fakes.map(Fakes::owners));
    let mut link = FakeLink::default();
    dispatcher.dispatch(frame, Instant::now(), &mut link);
    link.replies
}

fn install(request_id: &str, grant_id: &str) -> Down {
    Down::LocalTerminalGrant(DLocalTerminalGrant {
        request_id: request_id.to_owned(),
        grant_id: grant_id.to_owned(),
        ..Default::default()
    })
}

#[test]
fn an_accepted_install_is_acknowledged_with_its_grant_id() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let answers = dispatch(Some(&fakes), install("g1", "grant-1"));
    assert_eq!(
        answers,
        vec![Up::RpcOk { request_id: "g1".to_owned(), data: serde_json::json!({ "grant_id": "grant-1" }), trace_id: None }]
    );
    assert_eq!(fakes.log.calls(), ["local_terminal.install_grant:grant-1"]);
}

#[test]
fn a_refused_install_answers_the_stores_reason() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let answers = dispatch(Some(&fakes), install("g2", ""));
    assert_eq!(
        answers,
        vec![Up::RpcError { request_id: "g2".to_owned(), message: "grant_id is invalid".to_owned(), trace_id: None }]
    );
}

#[test]
fn a_worker_without_a_local_door_refuses_grants_as_v2_does() {
    let answers = dispatch(None, install("g3", "grant-3"));
    assert_eq!(
        answers,
        vec![Up::RpcError {
            request_id: "g3".to_owned(),
            message: "local terminal grants unsupported by this worker".to_owned(),
            trace_id: None,
        }]
    );
    let revoke = Down::LocalTerminalGrantRevoke(DLocalTerminalGrantRevoke { device_fingerprint: "d".repeat(64), ..Default::default() });
    assert!(dispatch(None, revoke).is_empty(), "v2's `local?.revokeDevice` is a no-op without a door");
}

#[test]
fn a_revocation_reaches_the_door_and_is_not_answered() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let device = "d".repeat(64);
    let revoke = Down::LocalTerminalGrantRevoke(DLocalTerminalGrantRevoke { device_fingerprint: device.clone(), ..Default::default() });
    assert!(dispatch(Some(&fakes), revoke).is_empty());
    assert_eq!(fakes.log.calls(), [format!("local_terminal.revoke_device:{device}")]);
}
