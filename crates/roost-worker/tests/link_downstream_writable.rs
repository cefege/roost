//! The link-side gates v2 keeps in `apps/worker/src/transport/coord-link-outbox.ts`,
//! through a real link loop: a cell sink that was refused is told the link is
//! writable (`maybeNotifyWritable`) only once the link is live, and once; compact
//! terminal metadata rides only a link that negotiated it and raw PTY metadata
//! only one that did not; the pong goes out before the link is live; a detach
//! drops the control lane.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::sync::Arc;
use std::time::Duration;

use link_downstream_support::live::{LiveLink, eventually, go_live, next_frame, send};
use link_downstream_support::{Fakes, OwnerMode};
use roost_protocol::cell::types::{CellGridFrame, CellRow, MouseTracking};
use roost_protocol::versioning::CAPABILITY_TERMINAL_METADATA_V1;
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::{
    Binary, CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, DIR_FROM_PTY,
    TerminalMetadata,
};
use roost_worker::runtime::link_loop::CoordinatorCellSink;
use roost_worker::runtime::link_wire::ProtoLinkWire;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};

fn frame() -> CellGridFrame {
    CellGridFrame {
        stream_id: "00000000-0000-4000-8000-0000000000a1".to_owned(),
        grid_epoch: "epoch-1".to_owned(),
        cols: 80,
        rows: 2,
        cursor_row: 0,
        cursor_col: 0,
        cursor_visible: true,
        alt_screen: false,
        cursor_keys_app: false,
        bracketed_paste: false,
        mouse_tracking: MouseTracking::None,
        mouse_sgr: false,
        focus_events: false,
        full: true,
        viewport_rows: (0..2)
            .map(|index| CellRow {
                index,
                spans: Arc::from(Vec::new()),
            })
            .collect(),
        scrollback_rows: Vec::new(),
        scrollback_append: Vec::new(),
        scrollback_total: 0,
        sb_base: 0,
        base_seq: 0,
        seq: 1,
    }
}

fn channel() -> ChannelId {
    ChannelId::try_from(4_i64).unwrap()
}

fn sentinel(request_id: &str) -> Up {
    Up::RpcOk {
        request_id: request_id.to_owned(),
        data: serde_json::Value::Null,
        trace_id: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_cell_sink_is_told_writable_once_and_only_after_the_link_is_live() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let sink = Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire)));
    let live = LiveLink::start(&fakes, Some(Arc::clone(&sink))).await;
    let timings = FrameTimings {
        pty_out_ms: 1,
        worker_emit_ms: 2,
    };
    assert_eq!(
        sink.send_frame(
            channel(),
            &frame(),
            &roost_worker::session::emit_frame::frame_wire(&frame(), timings).unwrap()
        ),
        CellSinkResult::Dropped
    );

    let mut socket = live.accept().await;
    // Several drain ticks pass while the barrier is short of live.
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fakes.log.count("lifecycle.on_writable"),
        0,
        "never before the link is live"
    );
    go_live(&mut socket, Vec::new()).await;
    eventually(|| fakes.log.count("lifecycle.on_writable") == 1).await;
    let calls = fakes.log.calls();
    let ready = calls
        .iter()
        .position(|call| call == "lifecycle.on_snapshot_ready")
        .unwrap();
    let writable = calls
        .iter()
        .position(|call| call == "lifecycle.on_writable")
        .unwrap();
    assert!(
        ready < writable,
        "the writable edge follows the snapshot: {calls:?}"
    );
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        fakes.log.count("lifecycle.on_writable"),
        1,
        "one refusal, one notification"
    );
    live.stop().await;
}

/// Send compact metadata, raw metadata and a control sentinel through the
/// uplink; return every frame that reached the coordinator, bounded.
async fn metadata_frames(capabilities: Vec<String>) -> Vec<Up> {
    let fakes = Fakes::new(OwnerMode::Answer);
    let live = LiveLink::start(&fakes, None).await;
    let mut socket = live.accept().await;
    go_live(&mut socket, capabilities).await;
    eventually(|| fakes.log.count("lifecycle.on_snapshot_ready") == 1).await;
    let metadata = TerminalMetadata {
        channel_id: channel(),
        title_changed: true,
        title: "vim".to_owned(),
        activity_changed: false,
        activity_ts_ms: 0,
    };
    assert!(live.uplink.send(Up::TerminalMetadata(metadata)));
    let raw = Binary {
        channel_id: channel(),
        direction: DIR_FROM_PTY,
        data: b"\x1b]0;vim\x07".to_vec(),
        seq: 9,
    };
    assert!(live.uplink.send(Up::Binary(raw)));
    assert!(live.uplink.send(sentinel("sentinel")));
    let mut frames = Vec::new();
    while let Ok(frame) =
        tokio::time::timeout(Duration::from_millis(300), next_frame(&mut socket)).await
    {
        frames.push(frame);
    }
    live.stop().await;
    frames
}

#[tokio::test(flavor = "multi_thread")]
async fn a_negotiated_link_carries_compact_metadata_and_drops_raw_metadata() {
    let frames = metadata_frames(vec![CAPABILITY_TERMINAL_METADATA_V1.to_owned()]).await;
    let mut kinds: Vec<_> = frames.iter().map(Up::kind).collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["rpc-ok", "terminal-metadata"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_without_metadata_carries_raw_metadata_and_drops_compact_metadata() {
    let frames = metadata_frames(Vec::new()).await;
    let mut kinds: Vec<_> = frames.iter().map(Up::kind).collect();
    kinds.sort_unstable();
    assert_eq!(kinds, ["binary", "rpc-ok"]);
}

/// v2 `drainQueues` writes the liveness lane before replay and before the link
/// is live (`coord-link-outbox.ts:168`, `send` routes `pong` there at `:233`):
/// a ping that arrives while the barrier waits on hello-ack is answered now.
#[tokio::test(flavor = "multi_thread")]
async fn a_ping_is_answered_before_the_link_is_live() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let live = LiveLink::start(&fakes, None).await;
    let mut socket = live.accept().await;
    assert!(matches!(next_frame(&mut socket).await, Up::Hello { .. }));
    send(
        &mut socket,
        &Down::Ping {
            ts: 7,
            trace_id: None,
        },
    )
    .await;
    assert_eq!(
        next_frame(&mut socket).await,
        Up::Pong {
            ts: 7,
            trace_id: None
        }
    );
    live.stop().await;
}

/// v2 `detachSocket` empties the control lane (`coord-link-outbox.ts:341`): a
/// reply queued for a connection that closed is never sent on the next one.
#[tokio::test(flavor = "multi_thread")]
async fn a_control_frame_queued_for_a_closed_connection_is_not_sent_on_the_next() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let live = LiveLink::start(&fakes, None).await;
    let mut first = live.accept().await;
    assert!(matches!(next_frame(&mut first).await, Up::Hello { .. }));
    assert!(live.uplink.send(sentinel("stale")));
    tokio::time::sleep(Duration::from_millis(100)).await;
    first.close(None).await.unwrap();
    drop(first);

    let mut second = live.accept().await;
    go_live(&mut second, Vec::new()).await;
    assert!(live.uplink.send(sentinel("fresh")));
    assert_eq!(
        next_frame(&mut second).await,
        sentinel("fresh"),
        "the stale reply died with its connection"
    );
    live.stop().await;
}
