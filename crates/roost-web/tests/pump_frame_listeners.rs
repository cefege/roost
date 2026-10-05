//! The pump's frames listener: the terminal painter's direct path from a
//! dispatch that moved a painted frame, with no reactive effect in between.
//!
//! A real `Pump` over a real `ClientCore`, its Sync link taken to ready through
//! the production handshake, then fed a full and a delta the way a socket does.
//!
//! Test root, so the unwrap allowance is declared here.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use dioxus::core::VNode;
use dioxus::prelude::{Element, VirtualDom};
use dioxus::signals::Signal;

use roost_client_core::effect::{Effect, RpcCall, RpcResult};
use roost_client_core::{ClientCore, ClientEvent, SyncDomain, SyncFrame};
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;

const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
const STREAM: &str = "00000000-0000-4000-8000-000000000001";
const GRID_EPOCH: &str = "g-1";
const ROWS: u32 = 4;

thread_local! {
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

fn pump_root() -> Element {
    BUILT.with(|built| {
        *built.borrow_mut() = Some(Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-1"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        ));
    });
    Ok(VNode::default())
}

/// A pump and the scope that owns its revision signal, dropped after it.
struct Fixture {
    pump: Pump,
    _dom: VirtualDom,
    generation: u64,
}

fn empty_hydration_answer(call: &RpcCall) -> Option<RpcResult> {
    Some(match call {
        RpcCall::SessionsList { call_id, .. } => RpcResult::SessionsList {
            call_id: *call_id,
            sessions: Default::default(),
            terminal_snapshot_token: Some("snapshot-token-1".to_owned()),
        },
        RpcCall::WorkersList { call_id } => RpcResult::WorkersList {
            call_id: *call_id,
            workers: Default::default(),
            routable_fps: Default::default(),
        },
        _ => return None,
    })
}

/// A pump whose Sync link is ready and whose replica expects `STREAM`. The
/// handshake runs on the core directly so the pump performs none of its effects.
fn ready_pump() -> Fixture {
    let mut dom = VirtualDom::new(pump_root);
    dom.rebuild_in_place();
    let pump = BUILT.with(|built| built.borrow_mut().take()).unwrap();
    let core = pump.core();
    let mut core = core.borrow_mut();
    let generation = match core.handle(ClientEvent::DialRequested).as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected exactly one dial, got {other:?}"),
    };
    core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: "sock-1".to_owned(),
        process_epoch: "epoch-1".to_owned(),
    });
    let effects = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::Subscribed {
            socket_id: "sock-1".to_owned(),
            process_epoch: "epoch-1".to_owned(),
            domains: vec![
                (SyncDomain::Terminal, 1, true),
                (SyncDomain::Workers, 1, true),
            ],
        },
    });
    for effect in &effects {
        if let Effect::Rpc(call) = effect
            && let Some(answer) = empty_hydration_answer(call)
        {
            core.handle(ClientEvent::RpcResultReceived(answer));
        }
    }
    let token = core.store().sync_terminal_token().unwrap();
    let replica = core.store_mut().terminal_mut(SESSION, "fp-1");
    replica.bind_generation(&token);
    replica.install_expected_stream(STREAM, 8, ROWS);
    replica.open_view("view-1", 8, ROWS, 0);
    drop(core);
    Fixture {
        pump,
        _dom: dom,
        generation,
    }
}

/// One cell frame; `base_seq == None` is a full of `ROWS` rows, otherwise a
/// one-row delta continuing it. Built through the event's own field types.
fn cell_frame(generation: u64, delivery_seq: u64, base_seq: Option<u64>) -> ClientEvent {
    let mut sync_frame = SyncFrame::CellGrid {
        session_id: SESSION.to_owned(),
        frame: Default::default(),
    };
    if let SyncFrame::CellGrid { frame, .. } = &mut sync_frame {
        frame.session_id = SESSION.to_owned();
        frame.stream_id = STREAM.to_owned();
        frame.grid_epoch = GRID_EPOCH.to_owned();
        frame.cols = 8;
        frame.rows = ROWS;
        frame.full = base_seq.is_none();
        frame.base_seq = base_seq.unwrap_or(0);
        frame.seq = base_seq.map_or(1, |base| base + 1);
        let painted = if base_seq.is_some() { 0..1 } else { 0..ROWS };
        for index in painted {
            frame.viewport_rows.push(Default::default());
            let row = frame.viewport_rows.last_mut().unwrap();
            row.index = index;
            row.spans.push(Default::default());
            let span = row.spans.last_mut().unwrap();
            span.text = format!("r{index}");
            span.fg = 256;
            span.bg = 256;
            span.columns = 1;
        }
    }
    ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq,
        frame: sync_frame,
    }
}

/// Counting listeners registered on both the frames and sweep lists.
fn counters(pump: &Pump) -> (Rc<Cell<u32>>, Rc<Cell<u32>>, u64) {
    let frames = Rc::new(Cell::new(0_u32));
    let sweeps = Rc::new(Cell::new(0_u32));
    let frame_count = Rc::clone(&frames);
    let token = pump.on_frames(Rc::new(move |_| frame_count.set(frame_count.get() + 1)));
    let sweep_count = Rc::clone(&sweeps);
    pump.on_sweep(Rc::new(move |_| sweep_count.set(sweep_count.get() + 1)));
    (frames, sweeps, token)
}

#[test]
fn a_moved_frame_calls_the_frames_listener_once_and_no_sweep_listener() {
    let fixture = ready_pump();
    let (frames, sweeps, _) = counters(&fixture.pump);
    let before = fixture.pump.core().borrow().store().frames_revision();
    let seen = Rc::new(Cell::new(0_u64));
    let seen_by_listener = Rc::clone(&seen);
    fixture
        .pump
        .on_frames(Rc::new(move |reading| seen_by_listener.set(reading)));

    fixture
        .pump
        .dispatch(cell_frame(fixture.generation, 1, None));

    let after = fixture.pump.core().borrow().store().frames_revision();
    assert!(after > before, "the full painted");
    assert_eq!(frames.get(), 1);
    assert_eq!(sweeps.get(), 0);
    assert_eq!(seen.get(), after, "the listener reads the store's counter");
}

#[test]
fn an_event_that_moves_only_the_revision_calls_no_listener() {
    let fixture = ready_pump();
    let (frames, sweeps, _) = counters(&fixture.pump);
    let before = fixture.pump.core().borrow().store().revision();

    fixture.pump.dispatch(ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: "fp-1".to_owned(),
        view_id: "view-2".to_owned(),
        cols: 8,
        rows: ROWS,
    });

    assert!(fixture.pump.core().borrow().store().revision() > before);
    assert_eq!(frames.get(), 0);
    assert_eq!(sweeps.get(), 0);
}

#[test]
fn a_removed_frames_listener_is_not_called() {
    let fixture = ready_pump();
    let (frames, sweeps, token) = counters(&fixture.pump);
    fixture
        .pump
        .dispatch(cell_frame(fixture.generation, 1, None));
    assert_eq!(frames.get(), 1);

    fixture.pump.remove_frame_listener(token);
    let before = fixture.pump.core().borrow().store().frames_revision();
    fixture
        .pump
        .dispatch(cell_frame(fixture.generation, 2, Some(1)));

    let after = fixture.pump.core().borrow().store().frames_revision();
    assert!(after > before, "the delta painted");
    assert_eq!(frames.get(), 1);
    assert_eq!(sweeps.get(), 0);
}
