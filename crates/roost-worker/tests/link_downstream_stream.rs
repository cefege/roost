//! Coordinator stream state keeps its own bounded admission (v2
//! `TERMINAL_STREAM_REQUEST_INFLIGHT_CAP` in
//! `apps/worker/src/transport/coord-link-downstream.ts`): the 65th request in
//! flight is refused before any write with v2's exact frame, a completed one
//! frees its slot, and a failed owner is answered as an ambiguous boundary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::time::Instant;

use link_downstream_support::{FakeLink, Fakes, OwnerMode, SESSION, next_uplink, settle_tasks};
use roost_proto::DTerminalStreamState;
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, TerminalStreamFailureKind,
    TerminalStreamResult, TerminalStreamStatus, TerminalWritePhase,
};
use roost_worker::runtime::downstream::Dispatcher;
use roost_worker::uplink::{TERMINAL_STREAM_REQUEST_INFLIGHT_CAP, channel};

fn stream_state(request_id: &str) -> Down {
    Down::TerminalStreamState(DTerminalStreamState {
        request_id: request_id.to_owned(),
        session_id: SESSION.to_owned(),
        stream_id: "stream-1".to_owned(),
        enabled: true,
        cols: 120,
        rows: 40,
        budget_ms: 8_000,
        ..Default::default()
    })
}

fn only_reply(link: &FakeLink) -> &TerminalStreamResult {
    let [Up::TerminalStreamResult(result)] = link.replies.as_slice() else {
        panic!(
            "exactly one synchronous stream result, got {:?}",
            link.replies
        )
    };
    result
}

#[tokio::test]
async fn the_65th_request_in_flight_is_refused_pre_write_and_a_completion_readmits() {
    assert_eq!(TERMINAL_STREAM_REQUEST_INFLIGHT_CAP, 64);
    let fakes = Fakes::new(OwnerMode::Hold);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    let mut link = FakeLink::default();
    for index in 0..TERMINAL_STREAM_REQUEST_INFLIGHT_CAP {
        dispatcher.dispatch(
            stream_state(&format!("held-{index}")),
            Instant::now(),
            &mut link,
        );
    }
    assert!(
        link.replies.is_empty(),
        "64 requests in flight are all admitted"
    );

    dispatcher.dispatch(stream_state("over"), Instant::now(), &mut link);
    let refusal = only_reply(&link);
    assert_eq!(refusal.request_id, "over");
    assert_eq!(refusal.session_id.as_str(), SESSION);
    assert_eq!(
        (refusal.stream_id.as_str(), refusal.enabled),
        ("stream-1", true)
    );
    assert_eq!(refusal.status, TerminalStreamStatus::Rejected);
    assert_eq!(refusal.phase, TerminalWritePhase::PreWrite);
    assert_eq!(
        refusal.failure_kind,
        Some(TerminalStreamFailureKind::RetryablePreWrite)
    );
    assert_eq!(refusal.reason, "worker terminal-stream admission is full");
    assert_eq!(
        (
            refusal.channel_resize_seq,
            refusal.effective_cols,
            refusal.effective_rows,
            refusal.resized
        ),
        (0, 0, 0, false),
        "a refusal before any write reports no geometry"
    );
    assert_eq!(
        fakes.log.count("stream.apply:"),
        TERMINAL_STREAM_REQUEST_INFLIGHT_CAP
    );

    fakes.gate.add_permits(1);
    let Up::TerminalStreamResult(committed) = next_uplink(&mut receiver).await else {
        panic!("a stream result")
    };
    assert_eq!(committed.status, TerminalStreamStatus::Committed);
    settle_tasks().await;

    dispatcher.dispatch(stream_state("readmitted"), Instant::now(), &mut link);
    assert_eq!(link.replies.len(), 1, "a completed request freed its slot");
    assert_eq!(fakes.log.count("stream.apply:readmitted"), 1);
}

#[tokio::test]
async fn a_committed_result_is_the_owners_own_frame() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    dispatcher.dispatch(stream_state("ok"), Instant::now(), &mut FakeLink::default());
    let Up::TerminalStreamResult(result) = next_uplink(&mut receiver).await else {
        panic!("a stream result")
    };
    assert_eq!(
        (
            result.request_id.as_str(),
            result.status,
            result.failure_kind
        ),
        ("ok", TerminalStreamStatus::Committed, None)
    );
    assert_eq!(
        (
            result.effective_cols,
            result.effective_rows,
            result.channel_resize_seq
        ),
        (120, 40, 3)
    );
}

#[tokio::test]
async fn a_failed_stream_owner_is_an_ambiguous_boundary_and_frees_its_slot() {
    let fakes = Fakes::new(OwnerMode::Panic);
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", Some(fakes.owners()));
    let mut link = FakeLink::default();
    for index in 0..=TERMINAL_STREAM_REQUEST_INFLIGHT_CAP {
        dispatcher.dispatch(
            stream_state(&format!("fails-{index}")),
            Instant::now(),
            &mut link,
        );
        let Up::TerminalStreamResult(result) = next_uplink(&mut receiver).await else {
            panic!("a stream result")
        };
        assert_eq!(result.status, TerminalStreamStatus::Ambiguous);
        assert_eq!(result.phase, TerminalWritePhase::Unknown);
        assert_eq!(
            result.failure_kind,
            Some(TerminalStreamFailureKind::AmbiguousBoundary)
        );
        assert_eq!(result.reason, "the fake owner failed on purpose");
        settle_tasks().await;
    }
    assert!(
        link.replies.is_empty(),
        "a panicked request never kept its slot"
    );
}

#[tokio::test]
async fn without_a_stream_owner_a_request_is_admitted_and_unanswered() {
    let (uplink, mut receiver) = channel();
    let dispatcher = Dispatcher::new(uplink, "epoch", None);
    let mut link = FakeLink::default();
    for index in 0..=TERMINAL_STREAM_REQUEST_INFLIGHT_CAP {
        dispatcher.dispatch(
            stream_state(&format!("none-{index}")),
            Instant::now(),
            &mut link,
        );
    }
    settle_tasks().await;
    assert!(
        link.replies.is_empty(),
        "v2 `onTerminalStreamState?.()` answers nothing and frees its slot"
    );
    assert!(receiver.try_recv().is_none());
}
