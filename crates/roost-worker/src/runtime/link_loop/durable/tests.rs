//! The two link-level rules the store cannot enforce for itself: that a
//! session's `opened` is offered to the coordinator before that session's
//! first cells, and that an un-acknowledged row is still waiting after the
//! link that wrote it is gone.
//! Both need the barrier and the authorisation slot, and neither is
//! reachable from outside the crate.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use roost_protocol::cell::types::{CellGridFrame, CellRow, MouseTracking};
use roost_protocol::wire::brand::{ChannelId, SessionId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::SessionKind;

use crate::event_store::database::DATABASE_FILE_NAME;
use crate::link_barrier::{Action, Barrier};
use crate::link_dial::CoordinatorEndpoint;
use crate::outbox::Lane;
use crate::runtime::credential::{CredentialError, CredentialSource};
use crate::runtime::link_loop::browser::BrowserLink;
use crate::runtime::link_loop::cell_sink::CoordinatorCellSink;
use crate::runtime::link_loop::{Authorised, LinkLoop, WorkerIdentity};
use crate::runtime::link_wire::ProtoLinkWire;
use crate::session::cell_sink::{CellSink, CellSinkResult, FrameTimings};

const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";
const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
const STREAM: &str = "00000000-0000-4000-8000-0000000000a1";

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("roost-durable-{}-{ordinal}", std::process::id()));
        std::fs::create_dir_all(&root).expect("a scratch directory is usable");
        Self(root)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A snapshot provider that answers, because the barrier refuses to leave
/// `replay` for `snapshot` without one and the point of the test is what
/// happens AFTER the snapshot commits, not that a missing provider wedges.
struct FixedSnapshot;

impl crate::runtime::snapshot_source::SnapshotSource for FixedSnapshot {
    fn is_active(&self) -> bool {
        true
    }

    fn snapshot(
        &self,
    ) -> Result<
        roost_protocol::wire::event::SessionEvent,
        crate::runtime::snapshot_source::SnapshotError,
    > {
        Ok(roost_protocol::wire::event::SessionEvent::Snapshot {
            worker_fp: WorkerFp::try_from(FINGERPRINT).expect("64 hex characters"),
            sessions: Vec::new(),
            ts: 0,
            trace_id: None,
        })
    }
}

fn delivery() -> std::sync::Arc<crate::session::durable_delivery::DurableDelivery> {
    std::sync::Arc::new(crate::session::durable_delivery::DurableDelivery::new())
}

struct FixedCredential;

impl CredentialSource for FixedCredential {
    fn mint(&self) -> Result<String, CredentialError> {
        Ok("a-test-credential".to_owned())
    }
}

fn link_for_test() -> LinkLoop {
    let endpoint =
        CoordinatorEndpoint::new("http://127.0.0.1:1", FINGERPRINT).expect("a usable endpoint");
    LinkLoop::new(
        endpoint,
        WorkerIdentity {
            worker_fp: WorkerFp::try_from(FINGERPRINT).expect("64 hex characters"),
            version: "test".to_owned(),
            process_epoch: "test-epoch".to_owned(),
        },
        std::sync::Arc::new(ProtoLinkWire),
        std::sync::Arc::new(FixedSnapshot),
        std::sync::Arc::new(FixedCredential),
        BrowserLink::detached(),
        crate::uplink::channel().1,
    )
}

fn opened() -> SessionEvent {
    SessionEvent::Opened {
        session_id: SessionId::try_from(SESSION).expect("a uuid is a session id"),
        worker_fp: WorkerFp::try_from(FINGERPRINT).expect("64 hex characters"),
        channel: ChannelId::try_from(1_i64).expect("a small channel id"),
        session_kind: SessionKind::Shell,
        cwd: "/home/user/project".to_owned(),
        ts: 1_700_000_000_000,
        trace_id: None,
    }
}

fn closed() -> SessionEvent {
    SessionEvent::Closed {
        session_id: SessionId::try_from(SESSION).expect("a uuid is a session id"),
        exit_code: Some(0),
        ts: 1_700_000_001_000,
        trace_id: None,
    }
}

/// A whole viewport, so the frame passes the protocol's own structure
/// admission. A stub would not: the sink is a production value and the only
/// way to see it work is to hand it a frame that is genuinely sendable.
fn full_frame() -> CellGridFrame {
    CellGridFrame {
        stream_id: STREAM.to_owned(),
        grid_epoch: "epoch-1".to_owned(),
        cols: 80,
        rows: 24,
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
        viewport_rows: (0..24)
            .map(|index| CellRow {
                index,
                // `CellRow`'s spans are shared with every other sink that
                // renders this frame, so they are an `Arc`, not a `Vec`.
                spans: std::sync::Arc::from(Vec::new()),
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

fn timings() -> FrameTimings {
    FrameTimings {
        pty_out_ms: 1_700_000_000_010,
        worker_emit_ms: 1_700_000_000_020,
    }
}

/// THE NAMED PROPERTY, at the level that enforces it.
///
/// A cell frame for a session the coordinator has not been told about is a
/// frame the browser cannot place, and the failure presents as a terminal
/// that never paints. So the two must be separated: the `opened` occupies
/// the ONE authorisation slot the drain empties ahead of every lane, the
/// cells wait in the lane the drain empties last, and the barrier cannot
/// reach `live` — and therefore cannot release the lanes — until the
/// acknowledgement for that `opened` has arrived.
#[tokio::test]
async fn an_opened_event_is_offered_before_that_sessions_first_cells() {
    let scratch = Scratch::new();
    let mut link = link_for_test();
    let journal = Journal::open(&scratch.0.join(DATABASE_FILE_NAME))
        .await
        .expect("a fresh outbox opens");
    link.attach_durable_outbox(std::sync::Arc::new(journal), delivery());
    let sink = std::sync::Arc::new(CoordinatorCellSink::new(std::sync::Arc::new(ProtoLinkWire)));
    // A sink is detached until the lifecycle attaches it at hello-ack; this
    // test drives the drain order, not the lifecycle.
    sink.set_attached(true);
    link.attach_cell_sink(std::sync::Arc::clone(&sink));

    let row = link
        .publish_durable_event(&opened())
        .await
        .expect("published");
    assert_eq!(row.client_seq, 1);
    assert_eq!(
        link.durable_pending(),
        1,
        "the writer holds no mirror of the row"
    );

    // The cells arrive, and they wait: not in the authorisation slot, which
    // the `opened` owns, but in the lane the drain reaches last.
    assert_eq!(
        sink.send_frame(
            ChannelId::try_from(1_i64).unwrap(),
            &full_frame(),
            timings()
        ),
        CellSinkResult::Sent
    );
    assert_eq!(link.move_cell_frames_into(), 1);
    assert_eq!(link.outbox.lane_len(Lane::Terminal), 1);
    assert!(link.authorised.is_none(), "the cell took the durable slot");

    // The coordinator acknowledges the hello, and the barrier releases the
    // `opened` into that slot — while still short of `live`.
    let action = link.pump.on_hello_ack();
    crate::runtime::link_drain::apply_to(&mut link, action);
    assert!(matches!(link.authorised, Some(Authorised::Durable(1))));
    // `Replay`, and not `Snapshot`: the barrier asks for a snapshot only once
    // the durable queue behind it has drained, so a pending `opened` holds the
    // link in the one state whose lanes it may not write. `Live` is the state
    // `move_cell_frames_into` depends on, and this is the assertion that the
    // cells are still shut out.
    assert_eq!(
        link.barrier(),
        Barrier::Replay,
        "the barrier left replay with an unacknowledged opened, so its cells were released ahead \
         of it"
    );
    assert_eq!(
        link.outbox.lane_len(Lane::Terminal),
        1,
        "the cells left early"
    );

    // The snapshot is acknowledged, and only now may the lanes drain.
    // The coordinator acknowledges the `opened` by its EXACT sequence. Only then
    // does the barrier ask for a snapshot, and only that request puts a sequence
    // of its own into the space — one after the durable event, so 2 here.
    let acked = link.pump.on_event_ack(1);
    // The barrier's answer is read before it is applied, because applying it
    // moves the value and what is under test is what the barrier SAID.
    assert!(
        matches!(acked, Action::WriteSnapshot),
        "the barrier did not ask for the snapshot once the opened was acknowledged, so it would \
         wait for a replay that is never coming"
    );
    crate::runtime::link_drain::apply_to(&mut link, acked);
    // The drain draws the snapshot's number from the outbox before framing it.
    link.authorise_snapshot().await;
    assert!(matches!(link.authorised, Some(Authorised::Snapshot(_))));

    // The snapshot draws from the SAME sequence space and is acknowledged from
    // it, which is what makes this socket generation routable at the coordinator.
    let action = link.pump.on_snapshot_ack(2);
    assert!(
        !matches!(action, Action::IgnoredAck { .. }),
        "the barrier ignored the coordinator's own snapshot acknowledgement, so it would wait for \
         one that is never coming"
    );
    crate::runtime::link_drain::apply_to(&mut link, action);
    assert!(link.barrier().allows_live_traffic());
    // `Authorised` carries a frame and so is `Debug` rather than `PartialEq`;
    // what the assertion is about is WHICH arm holds the slot.
    assert!(
        matches!(link.authorised, Some(Authorised::Snapshot(_))),
        "the snapshot did not take the slot the durable write had"
    );
}

/// A row the coordinator never acknowledged is still the outbox's head after
/// the link that wrote it is gone, and the barrier that comes back resumes
/// above it — so the replay is a REPLAY and not a second, new event.
#[tokio::test]
async fn an_un_acknowledged_row_is_still_waiting_for_the_next_link() {
    let scratch = Scratch::new();
    let first = {
        let mut link = link_for_test();
        let journal = Journal::open(&scratch.0.join(DATABASE_FILE_NAME))
            .await
            .expect("a fresh outbox opens");
        link.attach_durable_outbox(std::sync::Arc::new(journal), delivery());
        let row = link
            .publish_durable_event(&opened())
            .await
            .expect("published");
        // Nothing acknowledged. The link goes away anyway.
        assert_eq!(link.apply_durable_acks().await, 0);
        row.client_seq
    };

    let mut restarted = link_for_test();
    let journal = Journal::open(&scratch.0.join(DATABASE_FILE_NAME))
        .await
        .expect("the outbox reopens over its own file");
    let journal = std::sync::Arc::new(journal);
    restarted.attach_durable_outbox(std::sync::Arc::clone(&journal), delivery());
    let head = restarted
        .oldest_durable_row()
        .await
        .expect("read")
        .expect("the row is still waiting");
    assert_eq!(head.client_seq, first);
    assert_eq!(head.event, opened());

    // The next event continues the sequence, so the coordinator can tell the
    // replay from a new `open`.
    let next = restarted
        .publish_durable_event(&closed())
        .await
        .expect("published");
    // A BLOCK BOUNDARY, not `first + 1`: the outbox allocates sequences in
    // blocks of `SEQUENCE_BLOCK_SIZE`, so a restarted worker resumes at the top
    // of a reserved block. The invariant is that the new number is ABOVE the old
    // one — a repeat is what the barrier could not recover from — and that the
    // barrier was told it, which `publish_durable_event` does.
    assert!(
        next.client_seq > first,
        "the restarted outbox handed out {first} again, so the coordinator could not tell the \
         replay from a new open"
    );
    assert_eq!(
        next.client_seq % crate::event_store::SEQUENCE_BLOCK_SIZE,
        1,
        "the sequence is not on a block boundary, so the block claim is not what reserved it"
    );

    // And the exact acknowledgement retires that one row and only that one.
    restarted.note_durable_ack(next.client_seq);
    assert_eq!(restarted.apply_durable_acks().await, 1);
    let waiting = journal.pending().await.expect("read");
    assert_eq!(
        waiting.iter().map(|row| row.client_seq).collect::<Vec<_>>(),
        vec![first],
        "acknowledging the close also retired the open the coordinator never confirmed"
    );
}
