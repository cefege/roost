//! A dropped cell repair and a queued control reply do not block each other, and
//! the repair leads the reply: opened → full → RPC. Ports v2
//! `apps/worker/tests/transport/coord-link-repair-order.test.ts` through a real
//! link loop, a loopback coordinator and the production cell sink, with an
//! `on_writable` that sends the repair the way v2's test does.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;

use std::sync::{Arc, Mutex};

use link_downstream_support::live::{LiveLink, go_live, next_frame};
use link_downstream_support::{CallLog, Fakes, OwnerMode};
use roost_protocol::cell::types::{CellGridFrame, CellRow, MouseTracking};
use roost_protocol::wire::brand::ChannelId;
use roost_protocol::wire::coord_worker::CoordWorkerUpstream as Up;
use roost_worker::link_ports::LinkLifecyclePort;
use roost_worker::runtime::link_loop::CoordinatorCellSink;
use roost_worker::runtime::link_wire::ProtoLinkWire;
use roost_worker::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};

fn repair() -> CellGridFrame {
    CellGridFrame {
        stream_id: "00000000-0000-4000-8000-000000000777".to_owned(),
        grid_epoch: "repair-grid:0".to_owned(),
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
    ChannelId::try_from(7_i64).unwrap()
}

const TIMINGS: FrameTimings = FrameTimings {
    pty_out_ms: 1,
    worker_emit_ms: 2,
};

/// v2's test `onWritable` (`link.sendCellGrid(7, repair)`), with the sink
/// attached at hello-ack as the production cadence attaches it.
#[derive(Debug)]
struct RepairOnWritable {
    sink: Arc<CoordinatorCellSink>,
    log: CallLog,
    repair_result: Mutex<Option<CellSinkResult>>,
}

impl LinkLifecyclePort for RepairOnWritable {
    fn on_open(&self) {
        self.log.push("lifecycle.on_open");
    }
    fn on_hello_ack(&self, _: bool) {
        self.sink.set_attached(true);
    }
    fn on_detach(&self) {}
    fn on_writable(&self) {
        let sent = self.sink.send_frame(channel(), &repair(), TIMINGS);
        *self.repair_result.lock().unwrap() = Some(sent);
    }
    fn on_snapshot_ready(&self) {}
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_cell_repair_drains_before_a_queued_scrollback_reply() {
    let fakes = Fakes::new(OwnerMode::Answer);
    let sink = Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire)));
    let lifecycle = Arc::new(RepairOnWritable {
        sink: Arc::clone(&sink),
        log: fakes.log.clone(),
        repair_result: Mutex::new(None),
    });
    let mut owners = fakes.owners();
    owners.lifecycle = Arc::clone(&lifecycle) as Arc<dyn LinkLifecyclePort>;
    let live = LiveLink::start_with(owners, Some(Arc::clone(&sink))).await;
    let mut socket = live.accept().await;

    // A cell lost before the link could carry it arms an authoritative repair,
    // and the scrollback handler's reply queues behind the barrier.
    assert_eq!(
        sink.send_frame(channel(), &repair(), TIMINGS),
        CellSinkResult::Dropped
    );
    let reply = Up::RpcOk {
        request_id: "scrollback-request".to_owned(),
        data: serde_json::json!({ "rows": [], "cols": 80, "grid_epoch": "repair-grid:0" }),
        trace_id: None,
    };
    assert!(live.uplink.send(reply.clone()));

    go_live(&mut socket, Vec::new()).await;
    let first = next_frame(&mut socket).await;
    let second = next_frame(&mut socket).await;
    let Up::CellGrid(cells) = first else {
        panic!("the repair leads the reply: got {first:?} then {second:?}")
    };
    assert_eq!(cells.channel_id, 7);
    assert!(
        cells.frame.as_option().is_some_and(|frame| frame.full),
        "the repair is a full frame"
    );
    assert_eq!(second, reply, "the reply follows the repair, unchanged");
    assert_eq!(
        *lifecycle.repair_result.lock().unwrap(),
        Some(CellSinkResult::Sent)
    );
    live.stop().await;
}
