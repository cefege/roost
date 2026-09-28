//! The production cadence (`runtime::cell_cadence::CellCadence`): bytes a
//! keeper delivers reach a registered sink with no test calling
//! `emit_cell_frame`, and ports `apps/worker/tests/transport/coord-link-cell-sink.test.ts`
//! (a clean boot delivers, a link bounce costs one forced full) plus the
//! metadata lanes reaching the uplink.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "cell_support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use roost_observability::clock::{EventClock, SystemClock};
use roost_protocol::wire::coord_worker::{CoordWorkerDownstream, CoordWorkerUpstream};
use roost_worker::link_ports::LinkLifecyclePort;
use roost_worker::runtime::cell_cadence::CellCadence;
use roost_worker::runtime::cell_delivery::TableCellDelivery;
use roost_worker::runtime::channel_delivery::TableChannelDelivery;
use roost_worker::runtime::link_loop::CoordinatorCellSink;
use roost_worker::runtime::link_wire::{LinkWire, ProtoLinkWire, WireError};
use roost_worker::session::binding::{CellDelivery, ChannelDelivery};
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::lifecycle::SessionTable;
use roost_worker::uplink::{self, Uplink};
use support::{RecordFixture, ScriptedSink, channel, row_text, stream_id};

/// The link codec, recording every frame the coordinator sink encoded.
#[derive(Default)]
struct RecordingWire {
    encoded: Mutex<Vec<CoordWorkerUpstream>>,
}

impl RecordingWire {
    /// `full` for every cell-grid frame the coordinator sink was handed.
    fn cell_fulls(&self) -> Vec<bool> {
        self.encoded
            .lock()
            .unwrap()
            .iter()
            .filter_map(|frame| match frame {
                CoordWorkerUpstream::CellGrid(grid) => {
                    grid.frame.as_option().map(|frame| frame.full)
                }
                _ => None,
            })
            .collect()
    }
}

impl LinkWire for RecordingWire {
    fn encode_upstream(&self, frame: &CoordWorkerUpstream) -> Result<Vec<u8>, WireError> {
        self.encoded.lock().unwrap().push(frame.clone());
        ProtoLinkWire.encode_upstream(frame)
    }

    fn decode_downstream(&self, bytes: &[u8]) -> Result<CoordWorkerDownstream, WireError> {
        ProtoLinkWire.decode_downstream(bytes)
    }
}

struct Stack {
    _fixture: RecordFixture,
    table: Arc<SessionTable>,
    cells: TableCellDelivery,
    ingest: TableChannelDelivery,
    emitter: Arc<Mutex<CellEmitter>>,
}

fn stack(channel_number: i64) -> Stack {
    let fixture = RecordFixture::new();
    let table = Arc::new(SessionTable::default());
    table
        .insert(fixture.record(channel(channel_number), 80, 24))
        .expect("the table takes the record");
    let cells = TableCellDelivery::new(CellEmitter::new(), Arc::clone(&table));
    let emitter = cells.emitter();
    let ingest = TableChannelDelivery::new(
        Arc::clone(&emitter),
        Arc::new(roost_worker::session::terminal_changed::TerminalChangedHooks::default()),
    );
    Stack {
        _fixture: fixture,
        table,
        cells,
        ingest,
        emitter,
    }
}

impl Stack {
    fn deliver(&self, channel_number: u16, bytes: &[u8]) {
        let record = self
            .table
            .record_of_channel(channel_number)
            .expect("the record is live");
        let mut record = record.lock().unwrap();
        self.ingest
            .ingest_output(&mut record, bytes, SystemClock.now_epoch_ms());
    }

    fn cadence(
        &self,
        uplink: Uplink,
        wire: Arc<dyn LinkWire>,
    ) -> (CellCadence, Arc<CoordinatorCellSink>) {
        let coord = Arc::new(CoordinatorCellSink::new(wire));
        let cadence = CellCadence::new(
            Arc::clone(&self.emitter),
            Arc::clone(&self.table),
            Arc::new(SystemClock),
            uplink,
            Arc::clone(&coord),
        );
        (cadence, coord)
    }
}

#[tokio::test]
async fn ingested_bytes_reach_a_registered_sink_without_a_direct_emit() {
    let mut stack = stack(31);
    let coord = Arc::new(CoordinatorCellSink::new(Arc::new(ProtoLinkWire)));
    let (cadence, driver) = CellCadence::spawn(
        Arc::clone(&stack.emitter),
        Arc::clone(&stack.table),
        Arc::new(SystemClock),
        Uplink::detached(),
        coord,
    );
    // No coordinator is reachable: the link reports the socket gone.
    cadence.on_detach();
    let local = ScriptedSink::new("local:cadence");
    cadence.register_sink(local.clone());
    stack.cells.install_stream(channel(31), &stream_id(31));
    stack.deliver(31, b"\x1b[5;1Hcadence-marker");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let painted = loop {
        let painted = local
            .frames()
            .iter()
            .any(|frame| row_text(frame, 4).contains("cadence-marker"));
        if painted || tokio::time::Instant::now() > deadline {
            break painted;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    };
    driver.abort();
    assert!(
        painted,
        "ingested bytes never reached the registered sink: {:?}",
        local.fulls()
    );
}

#[tokio::test]
async fn a_clean_boot_delivers_cells_and_a_link_bounce_costs_one_forced_full() {
    let mut stack = stack(32);
    let wire = Arc::new(RecordingWire::default());
    let (cadence, coord) = stack.cadence(Uplink::detached(), wire.clone());

    cadence.on_open();
    cadence.on_hello_ack(true);
    cadence.on_snapshot_ready();
    stack.cells.install_stream(channel(32), &stream_id(1));
    cadence.run_pass();
    assert_eq!(wire.cell_fulls(), vec![true]);
    assert!(
        coord.frame_count() > 0,
        "the full never reached the link's queue"
    );

    stack.deliver(32, b"\x1b[2;1HLIVE");
    cadence.run_pass();
    assert_eq!(wire.cell_fulls(), vec![true, false]);

    cadence.on_detach();
    assert_eq!(
        coord.frame_count(),
        0,
        "a dead socket generation kept its queued cells"
    );
    stack.deliver(32, b"\x1b[3;1HDOWN");
    cadence.run_pass();
    assert_eq!(
        wire.cell_fulls(),
        vec![true, false],
        "a detached coordinator was handed a frame"
    );

    cadence.on_open();
    cadence.on_hello_ack(true);
    cadence.on_snapshot_ready();
    cadence.run_pass();
    assert_eq!(
        wire.cell_fulls(),
        vec![true, false, true],
        "a bounce must cost exactly one full"
    );
    let streams: Vec<String> = wire
        .encoded
        .lock()
        .unwrap()
        .iter()
        .filter_map(|frame| match frame {
            CoordWorkerUpstream::CellGrid(grid) => {
                grid.frame.as_option().map(|frame| frame.stream_id.clone())
            }
            _ => None,
        })
        .collect();
    assert!(
        streams.iter().all(|stream| *stream == stream_id(1)),
        "the bounce re-minted the stream"
    );
}

#[tokio::test]
async fn negotiated_titles_and_old_coordinator_raw_bytes_reach_the_uplink() {
    let stack = stack(33);
    let (uplink, mut receiver) = uplink::channel();
    let (cadence, _coord) = stack.cadence(uplink, Arc::new(ProtoLinkWire));

    cadence.on_open();
    stack.deliver(33, b"raw-before-ack");
    cadence.run_pass();
    let raw = receiver
        .try_recv()
        .expect("the raw lane dispatched to the uplink");
    assert!(
        matches!(&raw, CoordWorkerUpstream::Binary(binary) if binary.data == b"raw-before-ack"),
        "{raw:?}"
    );

    cadence.on_hello_ack(true);
    stack.deliver(33, b"\x1b]0;semantic title\x07");
    cadence.run_pass();
    let mut titles = Vec::new();
    while let Some(frame) = receiver.try_recv() {
        match frame {
            CoordWorkerUpstream::TerminalMetadata(metadata) if metadata.title_changed => {
                titles.push(metadata.title)
            }
            CoordWorkerUpstream::Binary(_) => panic!("raw bytes were sent after negotiation"),
            _ => {}
        }
    }
    assert_eq!(titles, vec!["semantic title".to_owned()]);
}

/// v2 `onWritable` → `resumeCellSink` drains the repair a refused coordinator
/// sink parked before it returns, which is what lets the link write that
/// repair ahead of its queued controls (`coord-link-repair-order.test.ts`).
#[tokio::test]
async fn the_repair_a_refused_coordinator_sink_is_owed_is_built_inside_on_writable() {
    let mut stack = stack(34);
    let wire = Arc::new(RecordingWire::default());
    let (cadence, coord) = stack.cadence(Uplink::detached(), wire.clone());
    cadence.on_open();
    cadence.on_hello_ack(true);
    cadence.on_snapshot_ready();
    stack.cells.install_stream(channel(34), &stream_id(1));
    cadence.run_pass();
    assert_eq!(wire.cell_fulls(), vec![true]);

    // The link cannot take the next frame, so the delta and its same-emit
    // repair are both refused and the sink is owed a writable notification.
    coord.set_attached(false);
    stack.deliver(34, b"\x1b[2;1HLOST");
    cadence.run_pass();
    assert!(coord.writable_owed());
    assert_eq!(
        wire.cell_fulls(),
        vec![true],
        "a refused frame never reached the codec"
    );

    coord.set_attached(true);
    assert!(coord.take_writable_owed());
    cadence.on_writable();
    assert_eq!(
        wire.cell_fulls(),
        vec![true, true],
        "the repair full exists when on_writable returns"
    );
}
