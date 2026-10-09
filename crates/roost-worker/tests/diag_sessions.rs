#![cfg(unix)]
//! The diagnostic snapshot's per-session entries are a WIRE CONTRACT, not a
//! report nicety. `diag.snapshot` fans out through the coordinator to every
//! layered terminal probe, and those readers resolve `sessions[sessionId]` to
//! `null` when it is absent — which a terminal stream reads as "the worker
//! stopped", not as "the worker said nothing useful". A snapshot without the
//! map is therefore indistinguishable from a stalled worker to every reader
//! downstream of it.
//!
//! What is pinned here is ADVANCE, not SHAPE. A shape-only assertion is
//! satisfied by a constant, and a constant is exactly what shipped broken: the
//! keys existed nowhere, and a fold that answered them with fixed numbers would
//! pass the same test while every probe still timed out. So each property is a
//! comparison against the live record, taken across a real change.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use roost_host::HostPlatform;
use roost_protocol::wire::brand::SessionId;
use roost_term::{CellEmitState, RioCore, TerminalCore};
use roost_worker::diag_snapshot::Snapshot;
use roost_worker::event_store::{DurableEventKind, Store};
use roost_worker::runtime::cell_delivery::TableCellDelivery;
use roost_worker::session::binding::CellDelivery;
use roost_worker::session::emit::CellEmitter;
use roost_worker::session::lifecycle::SessionTable;
use roost_worker::session::ring::ScrollbackRing;
use roost_worker::session::types::{SessionIdentity, SessionRecord};
use roost_worker::shell_spec::ShellSpec;

const SESSION: &str = "3f2504e0-4f89-41d3-9a0c-0305e82c3301";
const CHANNEL: i64 = 7;
const WORKER_FP: &str = "worker-fingerprint";

fn session_id() -> SessionId {
    SessionId::try_from(SESSION).expect("a uuid is a session id")
}

fn identity() -> SessionIdentity {
    SessionIdentity {
        session_id: session_id(),
        channel_id: CHANNEL.try_into().expect("a positive id is a channel id"),
        socket_path: "/run/roost/mux-keeper.sock".to_string(),
        cwd: "/home/almalinux/repos/roost".to_string(),
        shell_spec: ShellSpec {
            version: 1,
            platform: HostPlatform::Linux,
            executable: "/bin/bash".to_string(),
            argv: Vec::new(),
            cwd: "/home/almalinux/repos/roost".to_string(),
            env: vec![("TERM".to_string(), "xterm-256color".to_string())],
        },
        session_trace_id: "aabbccdd11223344".try_into().expect("hex is a trace id"),
        spawned_at_ms: 1_700_000_000_000,
    }
}

fn record() -> SessionRecord {
    let mut core = RioCore::new(24, 4);
    core.write(b"first line\r\n");
    let mut store = Store::new();
    let reservation = store
        .reserve(DurableEventKind::Closed, 64)
        .expect("the store admits a close claim");
    SessionRecord::new(
        identity(),
        reservation,
        Box::new(core),
        CellEmitState::new("epoch-base", "stream-1"),
        ScrollbackRing::new(1024),
    )
}

/// A live table and the delivery seam the snapshot reads its emitter facts
/// through — the three collaborators `Snapshot::of_live_sessions` names.
fn stack() -> (Arc<SessionTable>, Arc<Mutex<dyn CellDelivery>>) {
    let table = Arc::new(SessionTable::default());
    let held = table.insert(record()).expect("the table admits a session");
    held.lock()
        .expect("held")
        .append_retained(b"retained bytes");
    let cells: Arc<Mutex<dyn CellDelivery>> = Arc::new(Mutex::new(TableCellDelivery::new(
        CellEmitter::new(),
        Arc::clone(&table),
    )));
    (table, cells)
}

fn publish(table: &SessionTable, cells: &Mutex<dyn CellDelivery>) -> serde_json::Value {
    let snapshot = Snapshot::of_live_sessions(table, WORKER_FP, cells);
    snapshot.sessions[SESSION].clone()
}

/// A JSON NUMBER, and the failure says so. `cell.seq` is compared with
/// `BigInt(canonical.seq)` by the consumer; a string there is a coercion the
/// coord half tolerates and this half does not.
fn number(entry: &serde_json::Value, path: &[&str]) -> u64 {
    let mut cursor = entry;
    for key in path {
        cursor = &cursor[*key];
    }
    cursor
        .as_u64()
        .unwrap_or_else(|| panic!("{} must be a JSON NUMBER, was {cursor}", path.join(".")))
}

/// `raw.head_seq` is the byte head a reader compares across a publish. It must
/// ADVANCE, and it must advance to the record's own total — a value that merely
/// exists, or that repeats, leaves every probe waiting on a stream that never
/// moved.
#[test]
fn the_sessions_map_reports_a_byte_head_that_advances_with_the_record() {
    let (table, cells) = stack();
    let before = number(&publish(&table, &cells), &["raw", "head_seq"]);

    table
        .with_record_mut(&session_id(), |record| {
            record.append_retained(b"more bytes")
        })
        .expect("the table holds the session");

    let after = number(&publish(&table, &cells), &["raw", "head_seq"]);
    let expected = table
        .with_record(&session_id(), |record| record.head_seq)
        .expect("the table holds the session");

    assert!(
        after > before,
        "the byte head must advance: {before} then {after} describes a stream that never moved"
    );
    assert_eq!(
        after, expected,
        "and it must be the record's own total, not a number derived beside it"
    );
}

/// `cell.seq` must track the emit state rather than repeat, because a fold that
/// pinned it would satisfy a shape-only test while every convergence poll timed
/// out. The epoch beside it must be the one the record itself would stamp.
#[test]
fn the_sessions_map_reports_the_cell_sequence_the_emit_state_holds() {
    let (table, cells) = stack();
    let entry = publish(&table, &cells);
    let (seq, epoch) = table
        .with_record(&session_id(), |record| {
            (record.cell_emit.seq, record.cell_emit.grid_epoch())
        })
        .expect("the table holds the session");

    assert_eq!(
        number(&entry, &["cell", "seq"]),
        seq,
        "`cell.seq` is the emit state's own sequence"
    );
    assert_eq!(
        entry["cell"]["grid_epoch"].as_str(),
        Some(epoch.as_str()),
        "and the grid epoch is the one the record would stamp on a frame — an \
         empty one is a grid the consumer cannot fence against"
    );
}

/// The consumer keys every lookup on the session id, so a map keyed by
/// anything else is a map no reader can find an entry in.
#[test]
fn the_map_is_keyed_by_the_session_id_the_reader_asks_for() {
    let (table, cells) = stack();
    let snapshot = Snapshot::of_live_sessions(&table, WORKER_FP, &cells);
    assert_eq!(
        snapshot.sessions.keys().collect::<Vec<_>>(),
        vec![SESSION],
        "one entry per live session, under that session's own id"
    );
    assert_eq!(
        snapshot.sessions[SESSION]["channel_binding"]["worker_fp"].as_str(),
        Some(WORKER_FP),
        "a report names the machine it came from"
    );
}
