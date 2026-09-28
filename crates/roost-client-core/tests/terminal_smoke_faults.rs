//! The smoke frame faults end to end through `ClientCore::handle`: a blackhole
//! swallows every Sync cell frame of its generation and each is still
//! acknowledged, a wire-delta drop swallows exactly one delta, a fault armed
//! under a retired generation retires instead of firing, and nothing is armed
//! without a ready generation. Pins `terminal::smoke_faults` + `store::sync_smoke`
//! (v2 `apps/web/src/store/terminal-stream-diagnostics.ts:196-267`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::event::ClientEvent;
use roost_client_core::store::sync_smoke::{
    arm_terminal_blackhole, arm_terminal_wire_delta_drop, sync_redial_report,
};
use roost_client_core::sync::redial::SyncLinkLiveness;
use roost_client_core::terminal::smoke_faults::{FaultedFrameKind, TerminalSmokeFaults};
use roost_client_core::{ClientCore, SyncFrame};
use support::sync_reconnect::{acks, open_ready_link};
use support::{SESSION, STREAM, client, delta, full, sync_token};

/// A ready link and a replica bound to it, expecting `STREAM` at 8x4.
fn ready_client() -> (ClientCore, u64) {
    let mut core = client();
    let generation = open_ready_link(&mut core, "sock-1");
    let token = core
        .store()
        .sync_terminal_token()
        .expect("an open link has a token");
    let replica = core.store_mut().terminal_mut(SESSION, "fp-1");
    replica.bind_generation(&token);
    replica.install_expected_stream(STREAM, 8, 4);
    replica.open_view("view-1", 8, 4, 0);
    (core, generation)
}

fn deliver(core: &mut ClientCore, generation: u64, seq: u64, frame: SyncFrame) -> usize {
    acks(&core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: seq,
        frame,
    }))
}

fn cell(frame: roost_proto::PbCellGridFrame) -> SyncFrame {
    SyncFrame::CellGrid {
        session_id: SESSION.to_owned(),
        frame,
    }
}

fn frames_counted(core: &ClientCore) -> u64 {
    core.store()
        .terminal(SESSION)
        .unwrap()
        .frame_counts
        .frames()
}

#[test]
fn a_blackhole_swallows_every_frame_of_its_generation_and_each_is_still_acknowledged() {
    let (mut core, generation) = ready_client();
    assert!(arm_terminal_blackhole(core.store_mut(), SESSION));
    assert_eq!(deliver(&mut core, generation, 1, cell(full(4))), 1);
    assert_eq!(deliver(&mut core, generation, 2, cell(delta(1, 4, 0))), 1);
    assert_eq!(frames_counted(&core), 0);
    assert!(!core.store().is_paintable(SESSION));
    let counts = core.store().terminal_smoke_faults.counts(SESSION);
    assert_eq!(counts.blackhole_drop_count, 2);
}

#[test]
fn a_wire_delta_drop_swallows_exactly_one_delta_and_notes_the_next_sequence() {
    let (mut core, generation) = ready_client();
    deliver(&mut core, generation, 1, cell(full(4)));
    assert!(arm_terminal_wire_delta_drop(core.store_mut(), SESSION));
    assert_eq!(deliver(&mut core, generation, 2, cell(delta(1, 4, 0))), 1);
    assert_eq!(frames_counted(&core), 1);
    deliver(&mut core, generation, 3, cell(delta(2, 4, 1)));
    assert_eq!(frames_counted(&core), 2);

    let counts = core.store().terminal_smoke_faults.counts(SESSION);
    assert_eq!(counts.wire_delta_drop_count, 1);
    assert_eq!(counts.wire_delta_dropped_seq, Some(2));
    assert_eq!(counts.wire_delta_post_drop_seq, Some(3));
}

#[test]
fn a_wire_delta_drop_lets_a_full_through() {
    let (mut core, generation) = ready_client();
    assert!(arm_terminal_wire_delta_drop(core.store_mut(), SESSION));
    deliver(&mut core, generation, 1, cell(full(4)));
    assert_eq!(frames_counted(&core), 1);
    assert!(core.store().is_paintable(SESSION));
}

#[test]
fn a_fault_armed_under_a_retired_generation_retires_instead_of_firing() {
    let mut faults = TerminalSmokeFaults::default();
    faults.arm_blackhole(SESSION, sync_token(1, 1));
    faults.arm_wire_delta_drop(SESSION, sync_token(1, 1));
    let successor = sync_token(2, 1);
    assert!(!faults.consume(SESSION, &successor, FaultedFrameKind::Frame, false, Some(5)));
    assert!(!faults.consume(
        SESSION,
        &sync_token(1, 1),
        FaultedFrameKind::Frame,
        false,
        Some(6)
    ));
    assert_eq!(faults.counts(SESSION).blackhole_drop_count, 0);
    assert_eq!(faults.counts(SESSION).wire_delta_drop_count, 0);
}

#[test]
fn nothing_is_armed_for_a_session_with_no_generation() {
    let mut core = client();
    assert!(!arm_terminal_blackhole(core.store_mut(), SESSION));
    assert!(!arm_terminal_wire_delta_drop(core.store_mut(), SESSION));
}

#[test]
fn the_redial_report_reads_an_open_link_with_no_failures() {
    let (core, _) = ready_client();
    let report = sync_redial_report(core.store());
    assert_eq!(report.liveness, SyncLinkLiveness::Open);
    assert_eq!(report.failures, 0);
    assert!(!report.hidden_parked);
}
