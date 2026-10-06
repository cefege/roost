//! The two SYNCHRONOUS arms: terminal and semantic live frames, and the replies
//! that settle a request the coordinator is holding open.
//!
//! Neither arm may ask for a close it does not understand, and neither owns a
//! socket. The one that is easy to get wrong is the class check: a live frame
//! whose header and body disagree about its channel is REFUSED, not delivered to
//! whichever won, because a cell bound to a session nobody announced is the
//! history-corrupting drop this layer is here to prevent.
//!
//! `unwrap` and `expect` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to be
//! stated here. Every panic names a value the test just built.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod db_support;
mod frame_dispatch_support;
mod workers_support;

use std::sync::Arc;

use frame_dispatch_support::{LinkFixture, WORKER_FP, live_frame, rpc_frame};
use roost_coord::worker_link::dispatch::{DispatchOutcome, FrameDispatch};
use roost_protocol::wire::ChannelId;
use roost_protocol::wire::coord_worker::{CoordWorkerUpstream, TerminalMetadata};
use serde_json::json;

/// A cell grid on a channel, carrying no rows -- the arm under test is the
/// routing decision, not the fold.
fn cell_grid(channel: u32) -> CoordWorkerUpstream {
    CoordWorkerUpstream::CellGrid(roost_proto::WCellGrid {
        channel_id: channel,
        frame: roost_proto::buffa::MessageField::some(roost_proto::PbCellGridFrame::default()),
        ..Default::default()
    })
}

fn metadata(channel: i64, title: &str) -> CoordWorkerUpstream {
    CoordWorkerUpstream::TerminalMetadata(TerminalMetadata {
        channel_id: ChannelId::try_from(channel).expect("the fixture channel fits"),
        title_changed: true,
        title: title.to_owned(),
        activity_changed: false,
        activity_ts_ms: 1_000,
    })
}

#[tokio::test]
async fn a_cell_grid_on_a_channel_nothing_resolves_is_handled_and_counted() {
    let fixture = LinkFixture::new("live-unmapped").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();

    let outcome = dispatcher.handle_now(WORKER_FP, live_frame(7, cell_grid(7)));

    assert_eq!(
        outcome,
        DispatchOutcome::Handled,
        "the arm IS handled; the channel simply has no session yet, which is \
         the open race and not a refusal. Reporting Refused here would claim \
         the coordinator cannot place the frame, which is a different defect."
    );
}

#[tokio::test]
async fn a_live_frame_whose_header_and_body_name_different_channels_is_refused() {
    let fixture = LinkFixture::new("live-mapping").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();

    let outcome = dispatcher.handle_now(
        WORKER_FP,
        // The header says 9; the body says 7. Delivering to whichever won would
        // bind a PTY to a session nobody announced.
        live_frame(9, cell_grid(7)),
    );

    assert_eq!(outcome, DispatchOutcome::Refused);
}

#[tokio::test]
async fn a_cell_grid_whose_body_is_unset_is_refused_rather_than_claimed_handled() {
    let fixture = LinkFixture::new("live-empty").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();
    // A grid whose body is UNSET: the header arrived and the cells did not.
    // Refusing it is right, because "handled" would claim the coordinator
    // placed a frame that carried no cells at all.
    let empty = CoordWorkerUpstream::CellGrid(roost_proto::WCellGrid {
        channel_id: 4,
        frame: Default::default(),
        ..Default::default()
    });
    let outcome = dispatcher.handle_now(WORKER_FP, frame_dispatch_support::live_frame(4, empty));

    assert_eq!(outcome, DispatchOutcome::Refused);
}

#[tokio::test]
async fn a_semantic_metadata_frame_without_the_negotiated_capability_is_refused() {
    let fixture = LinkFixture::new("live-metadata-unnegotiated").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();

    let outcome = dispatcher.handle_now(WORKER_FP, live_frame(1, metadata(1, "vim")));

    assert_eq!(
        outcome,
        DispatchOutcome::Refused,
        "a worker that never advertised terminal_metadata_v1 gets no semantic hub"
    );
}

#[tokio::test]
async fn a_semantic_metadata_frame_on_an_unbound_channel_is_refused_not_attributed() {
    let fixture = LinkFixture::new("live-metadata-unmapped").await;
    fixture.mark_ready();
    let negotiated = Arc::new(roost_coord::coord_core::worker_handle::WorkerHandle::new(
        frame_dispatch_support::worker(WORKER_FP),
        None,
        "negotiated".to_owned(),
        std::collections::BTreeSet::from([
            roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1.to_owned(),
        ]),
        fixture.socket.sender(),
    ));
    roost_coord::workers::registry::claim_generation(
        &fixture.services.buses,
        &fixture.services.workers,
        Arc::clone(&negotiated),
    );
    let dispatcher = fixture.services.worker_dispatcher(Arc::clone(&negotiated));

    let outcome = dispatcher.handle_now(WORKER_FP, live_frame(1, metadata(1, "vim")));

    assert_eq!(
        outcome,
        DispatchOutcome::Refused,
        "a title for a channel nothing resolves has no session to name, and \
         attributing it to the wrong one is worse than dropping it"
    );
}

#[tokio::test]
async fn an_rpc_ok_settles_the_request_the_coordinator_is_holding_open() {
    let fixture = LinkFixture::new("rpc-ok").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();
    let mut pending = fixture
        .services
        .scrollback
        .pending()
        .create("rpc-1", Some(WORKER_FP), 1_000)
        .expect("the request is not already pending");

    let outcome = dispatcher.handle_now(
        WORKER_FP,
        rpc_frame(CoordWorkerUpstream::RpcOk {
            request_id: "rpc-1".to_owned(),
            data: json!({"cols": 120}),
            trace_id: None,
        }),
    );

    assert_eq!(outcome, DispatchOutcome::Handled);
    assert_eq!(
        pending.settle().await.expect("the caller is released"),
        json!({"cols": 120}),
        "the worker's own payload reaches the caller verbatim"
    );
}

#[tokio::test]
async fn an_rpc_error_rejects_the_request_rather_than_resolving_it() {
    let fixture = LinkFixture::new("rpc-error").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();
    let mut pending = fixture
        .services
        .scrollback
        .pending()
        .create("rpc-2", Some(WORKER_FP), 1_000)
        .expect("the request is not already pending");

    let outcome = dispatcher.handle_now(
        WORKER_FP,
        rpc_frame(CoordWorkerUpstream::RpcError {
            request_id: "rpc-2".to_owned(),
            message: "the keeper refused".to_owned(),
            trace_id: None,
        }),
    );

    assert_eq!(outcome, DispatchOutcome::Handled);
    let error = pending
        .settle()
        .await
        .expect_err("a rejection is not a value");
    assert!(
        error
            .message
            .as_deref()
            .unwrap_or_default()
            .contains("the keeper refused"),
        "the worker's own words reach the caller, got {:?}",
        error.message
    );
}

#[tokio::test]
async fn a_reply_nothing_is_waiting_on_is_handled_and_lost_rather_than_fatal() {
    let fixture = LinkFixture::new("rpc-unmatched").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();

    let outcome = dispatcher.handle_now(
        WORKER_FP,
        rpc_frame(CoordWorkerUpstream::RpcOk {
            request_id: "never-asked".to_owned(),
            data: json!({}),
            trace_id: None,
        }),
    );

    assert_eq!(
        outcome,
        DispatchOutcome::Handled,
        "a stale or late reply is dropped by the table, not by closing the link"
    );
}

#[tokio::test]
async fn an_arm_with_no_destination_is_refused_rather_than_claimed_handled() {
    let fixture = LinkFixture::new("live-unhandled").await;
    fixture.mark_ready();
    let dispatcher = fixture.dispatcher();

    // A pong is the keepalive's business, not a live frame's, and this
    // dispatcher must say so rather than silently doing nothing with it.
    let outcome = dispatcher.handle_now(
        WORKER_FP,
        live_frame(
            0,
            CoordWorkerUpstream::Pong {
                ts: 1_000,
                trace_id: None,
            },
        ),
    );

    assert_eq!(outcome, DispatchOutcome::Refused);
}
