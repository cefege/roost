//! An accepted view state installs the stream the authority minted, at the
//! authority's effective geometry — decoded from the coordinator's bytes and
//! applied through `ClientCore::handle`, as the TERM smoke flow receives it.
//!
//! Ported from v2 `apps/web/src/components/terminal/terminal-stream-view-commands.ts`
//! (`applyTerminalViewState`): the stream id and `effectiveCols`/`effectiveRows`
//! of the answer become the replica's expectation, and an answer without a valid
//! stream or geometry installs nothing.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod sync_decode_support;

use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain};
use roost_proto::__buffa::oneof::firehose_frame::Frame;
use roost_proto::{TerminalViewStateFrame, TerminalViewStatus};

use sync_decode_support::{SESSION, WORKER_FP, application, deliver, ready_core};

const VIEW: &str = "00000000-0000-4000-8000-0000000000c1";
const STREAM: &str = "00000000-0000-4000-8000-0000000000d1";

/// A ready client with one pane open at 80×24, its view command sent.
fn core_with_open_view() -> (ClientCore, u64) {
    let (mut core, generation) = ready_core();
    core.handle(ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: WORKER_FP.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 80,
        rows: 24,
    });
    (core, generation)
}

fn accepted(stream_id: &str, cols: u32, rows: u32) -> Frame {
    Frame::TerminalViewState(Box::new(TerminalViewStateFrame {
        view_id: VIEW.to_owned(),
        session_id: SESSION.to_owned(),
        status: TerminalViewStatus::Accepted.into(),
        stream_id: stream_id.to_owned(),
        effective_cols: cols,
        effective_rows: rows,
        ..TerminalViewStateFrame::default()
    }))
}

fn expectation(core: &ClientCore) -> (Option<String>, (u32, u32)) {
    let replica = core.store().terminal(SESSION).unwrap();
    (
        replica.expected_stream_id().map(str::to_owned),
        replica.effective_geometry(),
    )
}

#[test]
fn an_accepted_view_state_installs_its_stream_at_the_effective_geometry() {
    let (mut core, generation) = core_with_open_view();
    // Another, smaller view holds the session: the authority mints at the
    // minimum, not at this pane's 80×24.
    let frame = application(SyncDomain::Terminal, 1, accepted(STREAM, 60, 20));
    deliver(&mut core, generation, &frame);
    assert_eq!(
        expectation(&core),
        (Some(STREAM.to_owned()), (60, 20)),
        "the replica must expect the minted stream, or no baseline is ever admitted"
    );
}

#[test]
fn an_accepted_view_state_without_a_valid_stream_or_geometry_installs_nothing() {
    for (stream_id, cols, rows) in [("", 60, 20), ("not-a-uuid", 60, 20), (STREAM, 0, 20)] {
        let (mut core, generation) = core_with_open_view();
        let frame = application(SyncDomain::Terminal, 1, accepted(stream_id, cols, rows));
        deliver(&mut core, generation, &frame);
        assert_eq!(
            expectation(&core).0,
            None,
            "{stream_id:?} at {cols}×{rows} must install no stream"
        );
    }
}
