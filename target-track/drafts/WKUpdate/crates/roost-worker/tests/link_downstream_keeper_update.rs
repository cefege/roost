//! `keeperUpdatePrepare` routes to its owner: the owner is entered in receive
//! order (it closes channel admission synchronously), and its outcome is sent
//! later as v2's `rpc-ok` carrying the result or `rpc-error` carrying the
//! failure — a panicking owner included. Ports the `keeperUpdatePrepare` case
//! of `apps/worker/src/transport/coord-link-downstream.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, next_uplink};
use roost_proto::DKeeperUpdatePrepare;
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream as Down, CoordWorkerUpstream as Up};
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::channel;

fn prepare(request_id: &str, maintenance: bool) -> Down {
    Down::KeeperUpdatePrepare(DKeeperUpdatePrepare {
        request_id: request_id.to_owned(),
        maintenance,
        ..Default::default()
    })
}

#[tokio::test]
async fn the_owner_is_entered_in_receive_order_and_its_result_is_an_rpc_ok() {
    let fakes = Fakes::new(OwnerMode::Hold);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    let mut link = FakeLink::default();
    dispatcher.dispatch(prepare("k1", true), Instant::now(), &mut link);
    assert_eq!(fakes.log.calls(), vec!["keeper_update.prepare:k1".to_owned()]);
    assert!(link.replies.is_empty(), "the answer waits for the owner");
    fakes.gate.add_permits(1);
    assert_eq!(
        next_uplink(&mut receiver).await,
        Up::RpcOk {
            request_id: "k1".to_owned(),
            data: serde_json::json!({ "outcome": "shutdown" }),
            trace_id: None,
        }
    );
}

#[tokio::test]
async fn a_failed_preparation_is_an_rpc_error_carrying_its_message() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    dispatcher.dispatch(prepare("k2", false), Instant::now(), &mut FakeLink::default());
    assert_eq!(
        next_uplink(&mut receiver).await,
        Up::RpcError {
            request_id: "k2".to_owned(),
            message: "journaled keeper update request is malformed".to_owned(),
            trace_id: None,
        }
    );
}

#[tokio::test]
async fn a_panicking_owner_is_answered_once_with_its_message() {
    let fakes = Fakes::new(OwnerMode::Panic);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    dispatcher.dispatch(prepare("k3", true), Instant::now(), &mut FakeLink::default());
    assert_eq!(
        next_uplink(&mut receiver).await,
        Up::RpcError {
            request_id: "k3".to_owned(),
            message: "the fake owner failed on purpose".to_owned(),
            trace_id: None,
        }
    );
}
