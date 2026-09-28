//! Journal → link durable delivery against a loopback coordinator: rows the
//! durable sink writes reach the coordinator under their stored `client_seq`
//! (live and after a restart), the snapshot is `Event{snapshot, client_seq}`
//! numbered from the same outbox, the replay barrier the boot reconcile awaits,
//! and boot's hold on the snapshot. Ports v2 `apps/worker/src/transport/event-sink.ts`
//! `coordLinkSink`, `coord-link-unacked.ts` and `coord-link-replay-barrier.ts`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod link_downstream_support;
#[path = "credential_support/scratch.rs"]
mod scratch;

use std::sync::Arc;
use std::time::Duration;

use link_downstream_support::live::{
    FINGERPRINT, LiveLink, Socket, next_bytes, next_frame, send, snapshot_event,
};
use link_downstream_support::{Fakes, OwnerMode};
use roost_protocol::wire::brand::{ChannelId, SessionId, WorkerFp};
use roost_protocol::wire::coord_worker::{
    CoordWorkerDownstream as Down, CoordWorkerUpstream as Up, EventAck,
};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::SessionKind;
use roost_worker::event_store::{DATABASE_FILE_NAME, DurableEventKind, Journal};
use roost_worker::runtime::snapshot_source::SnapshotActivation;
use roost_worker::session::durable_delivery::DurableDelivery;
use roost_worker::session::journal_sink::JournalSink;
use roost_worker::session::sinks::SessionEventSink;
use scratch::Scratch;

const FIRST: &str = "00000000-0000-4000-8000-00000000beef";
const SECOND: &str = "00000000-0000-4000-8000-00000000cafe";
/// Long enough for a live link to have drained several times over.
const SETTLE: Duration = Duration::from_millis(300);

fn opened(session: &str) -> SessionEvent {
    SessionEvent::Opened {
        session_id: SessionId::try_from(session).unwrap(),
        worker_fp: WorkerFp::try_from(FINGERPRINT).unwrap(),
        channel: ChannelId::try_from(1_i64).unwrap(),
        session_kind: SessionKind::Shell,
        cwd: "/home/user/project".to_owned(),
        ts: 1_700_000_000_000,
        trace_id: None,
    }
}

async fn open_outbox(scratch: &Scratch) -> Arc<Journal> {
    Arc::new(
        Journal::open(&scratch.path(DATABASE_FILE_NAME))
            .await
            .unwrap(),
    )
}

/// The durable sink the session layer writes through, as boot builds it.
fn sink_over(journal: &Arc<Journal>, delivery: &Arc<DurableDelivery>) -> JournalSink {
    JournalSink::new(Arc::clone(journal), Arc::clone(delivery))
}

async fn emit_opened(sink: &JournalSink, session: &str) {
    let claim = sink.reserve(DurableEventKind::Opened).await.unwrap();
    sink.emit(&opened(session), Some(claim)).await.unwrap();
}

struct DurableLink {
    live: LiveLink,
    journal: Arc<Journal>,
    delivery: Arc<DurableDelivery>,
    sink: JournalSink,
    snapshot: Option<SnapshotActivation>,
}

async fn start(journal: Arc<Journal>) -> DurableLink {
    start_link(journal, false).await
}

/// `held`: boot's hold on the snapshot, released by `DurableLink::snapshot`.
async fn start_link(journal: Arc<Journal>, held: bool) -> DurableLink {
    let delivery = Arc::new(DurableDelivery::new());
    let sink = sink_over(&journal, &delivery);
    let (outbox, signal) = (Arc::clone(&journal), Arc::clone(&delivery));
    let mut snapshot = None;
    let slot = &mut snapshot;
    let live = LiveLink::start_configured(Fakes::new(OwnerMode::Answer).owners(), move |link| {
        link.attach_durable_outbox(outbox, signal);
        if held {
            *slot = Some(link.hold_snapshot_until_activated());
        }
    })
    .await;
    DurableLink {
        live,
        journal,
        delivery,
        sink,
        snapshot,
    }
}

async fn hello_ack(socket: &mut Socket) {
    assert!(matches!(next_frame(socket).await, Up::Hello { .. }));
    let ack = Down::HelloAck {
        capabilities: Vec::new(),
        trace_id: None,
    };
    send(socket, &ack).await;
}

async fn next_event(socket: &mut Socket) -> (SessionEvent, u64) {
    match next_frame(socket).await {
        Up::Event {
            event, client_seq, ..
        } => (event, client_seq),
        other => panic!("expected a sequenced event, got {other:?}"),
    }
}

async fn ack(socket: &mut Socket, client_seq: u64) {
    send(socket, &Down::EventAck(EventAck { client_seq })).await;
}

/// The next frame is the snapshot; its sequence is returned unacknowledged.
async fn snapshot_seq(socket: &mut Socket) -> u64 {
    let (event, client_seq) = next_event(socket).await;
    assert_eq!(event, snapshot_event(), "expected the snapshot");
    client_seq
}

async fn pending_rows(journal: &Journal) -> Vec<u64> {
    let rows = journal.pending().await.unwrap();
    rows.into_iter().map(|row| row.client_seq).collect()
}

async fn eventually_empty(journal: &Journal) {
    for _ in 0..400 {
        if pending_rows(journal).await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("the coordinator's acknowledgements never retired the outbox rows");
}

async fn eventually_drained(delivery: &DurableDelivery) {
    tokio::time::timeout(Duration::from_secs(10), delivery.wait_for_replay())
        .await
        .expect("the durable replay never drained")
        .expect("the link was disposed while it replayed");
}

/// v2 `coordLinkSink`: a row the sink wrote before the hello-ack replays
/// first; one written while live goes out at once; each ack retires its row.
#[tokio::test]
async fn sink_rows_reach_the_coordinator_before_the_snapshot_and_while_live() {
    let scratch = Scratch::new("durable-rows");
    let link = start(open_outbox(&scratch).await).await;
    let mut socket = link.live.accept().await;
    emit_opened(&link.sink, FIRST).await;
    hello_ack(&mut socket).await;

    let (event, first_seq) = next_event(&mut socket).await;
    assert_eq!(event, opened(FIRST), "the replay carries the stored row");
    assert_eq!(pending_rows(&link.journal).await, [first_seq]);
    ack(&mut socket, first_seq).await;
    let snapshot = snapshot_seq(&mut socket).await;
    ack(&mut socket, snapshot).await;
    eventually_empty(&link.journal).await;

    emit_opened(&link.sink, SECOND).await;
    let (event, second_seq) = next_event(&mut socket).await;
    assert_eq!(
        event,
        opened(SECOND),
        "a live row goes out without a new snapshot"
    );
    ack(&mut socket, second_seq).await;
    eventually_empty(&link.journal).await;
    link.live.stop().await;
}

/// v2 `coord-link-unacked.ts` `oldestDurable`: rows a previous process left
/// unacknowledged replay oldest first under their stored sequences, before a
/// snapshot numbered above the sequence block this process took over.
#[tokio::test]
async fn a_restart_replays_the_unacknowledged_rows_before_its_snapshot() {
    let scratch = Scratch::new("durable-restart");
    let left = {
        let journal = open_outbox(&scratch).await;
        let sink = sink_over(&journal, &Arc::new(DurableDelivery::new()));
        emit_opened(&sink, FIRST).await;
        emit_opened(&sink, SECOND).await;
        let left = pending_rows(&journal).await;
        journal.close().await.unwrap();
        left
    };
    assert_eq!(left.len(), 2);

    let link = start(open_outbox(&scratch).await).await;
    let mut socket = link.live.accept().await;
    hello_ack(&mut socket).await;
    for (session, stored) in [FIRST, SECOND].into_iter().zip(&left) {
        let (event, client_seq) = next_event(&mut socket).await;
        assert_eq!((event, client_seq), (opened(session), *stored));
        ack(&mut socket, client_seq).await;
    }
    let snapshot = snapshot_seq(&mut socket).await;
    assert!(
        snapshot > left[1],
        "the snapshot reused a replayed sequence"
    );
    ack(&mut socket, snapshot).await;
    eventually_empty(&link.journal).await;
    link.live.stop().await;
}

/// v2 `startSnapshotBarrier` draws `store.nextClientSeq()`: the snapshot and
/// a journal row never share a sequence, and a committed row the link has not
/// offered keeps the snapshot from being numbered at all.
#[tokio::test]
async fn the_snapshot_and_a_journal_row_never_share_a_sequence() {
    let scratch = Scratch::new("durable-seq");
    let link = start(open_outbox(&scratch).await).await;
    let mut socket = link.live.accept().await;
    emit_opened(&link.sink, FIRST).await;
    hello_ack(&mut socket).await;
    let (_, before) = next_event(&mut socket).await;
    ack(&mut socket, before).await;
    let snapshot = snapshot_seq(&mut socket).await;
    ack(&mut socket, snapshot).await;
    emit_opened(&link.sink, SECOND).await;
    let (_, after) = next_event(&mut socket).await;
    assert!(
        before < snapshot && snapshot < after,
        "{before} {snapshot} {after}"
    );
    ack(&mut socket, after).await;
    link.live.stop().await;

    let draw_scratch = Scratch::new("durable-draw");
    let journal = open_outbox(&draw_scratch).await;
    let sink = sink_over(&journal, &Arc::new(DurableDelivery::new()));
    emit_opened(&sink, FIRST).await;
    let row = pending_rows(&journal).await[0];
    assert_eq!(journal.snapshot_sequence(row - 1).await.unwrap(), None);
    let drawn = journal.snapshot_sequence(row).await.unwrap().unwrap();
    emit_opened(&sink, SECOND).await;
    assert!(row < drawn && drawn < pending_rows(&journal).await[1]);
}

/// v2 `waitForDurableSessionEventReplay`: resolves once every row is acked and
/// the snapshot stage is reached; an append makes it pending at once, and a
/// live link with a claim for an event not yet written stays pending.
#[tokio::test]
async fn the_replay_barrier_waits_for_acked_rows_and_unblocked_claims() {
    let scratch = Scratch::new("durable-barrier");
    let link = start(open_outbox(&scratch).await).await;
    let mut socket = link.live.accept().await;
    emit_opened(&link.sink, FIRST).await;
    let waiter = tokio::spawn({
        let delivery = Arc::clone(&link.delivery);
        async move { delivery.wait_for_replay().await }
    });
    hello_ack(&mut socket).await;
    let (_, first) = next_event(&mut socket).await;
    tokio::time::sleep(SETTLE).await;
    assert!(!waiter.is_finished(), "drained over an unacknowledged row");
    ack(&mut socket, first).await;
    let snapshot = snapshot_seq(&mut socket).await;
    tokio::time::timeout(Duration::from_secs(10), waiter)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    ack(&mut socket, snapshot).await;

    emit_opened(&link.sink, SECOND).await;
    assert!(
        !link.delivery.is_drained(),
        "an append left the barrier drained"
    );
    let (_, second) = next_event(&mut socket).await;
    ack(&mut socket, second).await;
    eventually_drained(&link.delivery).await;

    let claim = link.sink.reserve(DurableEventKind::Closed).await.unwrap();
    tokio::time::sleep(SETTLE).await;
    assert!(!link.delivery.is_drained(), "drained over a blocking claim");
    link.sink.release(claim).await;
    eventually_drained(&link.delivery).await;
    link.live.stop().await;
}

/// v2 `hasBlockingSessionEventReservation`: a claim for an event that does not
/// exist yet holds the snapshot back until it is held.
#[tokio::test]
async fn a_blocking_claim_holds_the_snapshot_until_it_is_held() {
    let scratch = Scratch::new("durable-claim");
    let link = start(open_outbox(&scratch).await).await;
    let mut socket = link.live.accept().await;
    let claim = link.sink.reserve(DurableEventKind::Closed).await.unwrap();
    hello_ack(&mut socket).await;
    let early = tokio::time::timeout(SETTLE, next_bytes(&mut socket)).await;
    assert!(
        early.is_err(),
        "a frame went out while a claim blocked the snapshot"
    );
    link.sink.hold(claim).await;
    let snapshot = snapshot_seq(&mut socket).await;
    ack(&mut socket, snapshot).await;
    eventually_drained(&link.delivery).await;
    link.live.stop().await;
}

/// v2 boot dials and replays before its reconcile and activates the snapshot
/// provider only after it: a held link drains its replay at the snapshot stage
/// without publishing, a row appended meanwhile goes out at once (v2 `send`
/// turns an unsent snapshot phase back into replay), and the snapshot follows
/// the activation.
#[tokio::test]
async fn a_held_snapshot_drains_the_replay_and_publishes_only_once_activated() {
    let scratch = Scratch::new("durable-held");
    let link = start_link(open_outbox(&scratch).await, true).await;
    let snapshot = link.snapshot.clone().unwrap();
    let mut socket = link.live.accept().await;
    emit_opened(&link.sink, FIRST).await;
    hello_ack(&mut socket).await;
    let (_, first) = next_event(&mut socket).await;
    ack(&mut socket, first).await;
    eventually_drained(&link.delivery).await;
    let early = tokio::time::timeout(SETTLE, next_bytes(&mut socket)).await;
    assert!(early.is_err(), "the held snapshot was published");

    emit_opened(&link.sink, SECOND).await;
    let (event, second) = next_event(&mut socket).await;
    assert_eq!(event, opened(SECOND), "a row appended while held waited");
    ack(&mut socket, second).await;
    eventually_drained(&link.delivery).await;

    snapshot.activate();
    let published = snapshot_seq(&mut socket).await;
    assert!(published > second, "{published} {second}");
    ack(&mut socket, published).await;
    link.live.stop().await;
}
