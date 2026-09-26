//! Shared fixtures for the client-core behaviour tests: hand-built protobuf cell
//! frames, a bound replica, and an in-memory client.
//!
//! The frames are assembled directly rather than encoded and decoded, so each
//! contract rule can be broken in isolation and the code that notices it named in
//! the failure. Mirrors `crates/roost-protocol/tests/support/cell_chunks.rs`.

#![allow(dead_code)]

use roost_client_core::{
    ClientCore, MemoryClock, MemoryKeyValueStore, TerminalSession, TerminalToken,
};
use roost_proto::{PbCellGridFrame, PbCellRow, PbCellSpan};

/// The stream the fixtures install an expectation for.
pub const STREAM: &str = "00000000-0000-4000-8000-000000000001";
/// A second stream, for the "this frame is not mine" cases.
pub const OTHER_STREAM: &str = "00000000-0000-4000-8000-000000000002";
/// The session every fixture frame belongs to.
pub const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
/// The grid epoch a valid frame carries.
pub const EPOCH: &str = "g-1";
/// The grid epoch a resize mints.
pub const NEXT_EPOCH: &str = "g-2";

/// One styled cell run.
pub fn span(text: &str) -> PbCellSpan {
    PbCellSpan {
        text: text.to_owned(),
        fg: 256,
        bg: 256,
        columns: 1,
        ..Default::default()
    }
}

/// One row carrying one run.
pub fn row(index: u32, text: &str) -> PbCellRow {
    PbCellRow {
        index,
        spans: vec![span(text)],
        ..Default::default()
    }
}

/// A complete authoritative full: `rows` viewport rows of `r<index>`, no history.
pub fn full(rows: u32) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: SESSION.to_owned(),
        stream_id: STREAM.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols: 8,
        rows,
        full: true,
        viewport_rows: (0..rows)
            .map(|index| row(index, &format!("r{index}")))
            .collect(),
        seq: 1,
        ..Default::default()
    }
}

/// A delta that exactly continues `base_seq`, changing one row.
pub fn delta(base_seq: u64, rows: u32, changed_row: u32) -> PbCellGridFrame {
    PbCellGridFrame {
        session_id: SESSION.to_owned(),
        stream_id: STREAM.to_owned(),
        grid_epoch: EPOCH.to_owned(),
        cols: 8,
        rows,
        full: false,
        viewport_rows: vec![row(changed_row, "changed")],
        base_seq,
        seq: base_seq + 1,
        ..Default::default()
    }
}

/// The Sync generation a fixture replica is bound to.
pub fn sync_token(socket_generation: u64, domain_generation: u64) -> TerminalToken {
    TerminalToken::sync(
        socket_generation,
        format!("sock-{socket_generation}"),
        "epoch-1",
        domain_generation,
    )
}

/// A replica with the expectation installed, the generation bound, and one view
/// open, so a fixture frame has somewhere to land.
pub fn bound_replica(rows: u32) -> TerminalSession {
    let mut replica = TerminalSession::new(SESSION, "fp-1");
    replica.bind_generation(&sync_token(1, 1));
    replica.install_expected_stream(STREAM, 8, rows);
    replica.open_view("view-1", 8, rows, 0);
    replica
}

/// A replica that has already accepted a complete full, so a delta is admissible.
pub fn replica_with_baseline(rows: u32) -> TerminalSession {
    let mut replica = bound_replica(rows);
    let token = sync_token(1, 1);
    let outcome = replica.admit_frame(&full(rows), false, &token, 0);
    assert_eq!(
        outcome,
        roost_client_core::Admission::BaselineReplaced,
        "the fixture's own full must be admissible, or every other assertion is noise"
    );
    replica
}

/// A client over the in-memory host.
pub fn client() -> ClientCore {
    ClientCore::in_memory("tab-1")
}

/// A client whose clock the test can move.
pub fn client_with_clock() -> (ClientCore, std::rc::Rc<MemoryClock>) {
    let clock = std::rc::Rc::new(MemoryClock::new());
    let core = ClientCore::new(
        clock.clone(),
        std::rc::Rc::new(MemoryKeyValueStore::new()),
        "tab-1",
    );
    (core, clock)
}
