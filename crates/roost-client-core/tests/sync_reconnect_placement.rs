//! A reconnect is a GENERATION, not a retry, and a frame queued across one must
//! still be placeable when it is finally read.
//!
//! Two properties, each with the v2 rule it ports
//! (`apps/web/src/client/sync/sync-flow.ts`, `apps/web/src/store/sync.ts`):
//!
//! 1. A queued frame keeps its delivery sequence, its socket id and its lane, so
//!    it can be applied after the socket that delivered it is gone. Dispatch is
//!    deliberately NOT generation gated; only the cumulative acknowledgement
//!    is.
//! 2. State belonging to a previous generation is refused once a new one opens:
//!    a stale close must not retire a live link, nor latch a verdict that
//!    belonged to a socket nobody is using. The close codes themselves are in
//!    `sync_close_codes.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;

use roost_client_core::client::sync::{
    AbortReason, EnqueueOutcome, InstalledLink, QueuedFrame, SYNC_DISPATCH_QUEUE_MAX, SyncDispatch,
    can_accept_sync_link, can_open_sync_link, classify_close,
};
use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain, SyncFrame, WireEvent};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, SessionKind, WorkerFp};

const TAB: &str = "tab-7f3a";
const EPOCH: &str = "epoch-1";
const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The domain generation every fixture link announces.
const DOMAIN_GENERATION: u64 = 1;

fn session_event(event_id: u64) -> SyncFrame {
    SyncFrame::SessionEvent {
        event: WireEvent(SessionEvent::Opened {
            session_id: SessionId::try_from(SESSION).expect("a valid session id"),
            worker_fp: WorkerFp::try_from(WORKER_FP).expect("a valid fingerprint"),
            channel: ChannelId::try_from(0_i64).expect("a valid channel"),
            session_kind: SessionKind::Shell,
            cwd: "/repo".to_owned(),
            ts: 1,
            trace_id: None,
        }),
        event_id,
    }
}

/// Dial, complete the handshake, announce the workers domain, hydrate, and close
/// that domain's snapshot/live gap. Returns the generation the socket took.
///
/// A second call replaces the first socket's link, so a caller that wants the old
/// one still open has to say so itself: `SyncState::open_link` refuses to install
/// a second live link, which is what stops two sockets being current at once.
fn open_ready_link(core: &mut ClientCore, socket_id: &str) -> u64 {
    let effects = core.handle(ClientEvent::DialRequested);
    let generation = match effects.as_slice() {
        [Effect::DialSync { generation, .. }] => *generation,
        other => panic!("expected exactly one dial, got {other:?}"),
    };
    core.handle(ClientEvent::SyncLinkOpened {
        generation,
        socket_id: socket_id.to_owned(),
        process_epoch: EPOCH.to_owned(),
    });
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::Subscribed {
            socket_id: socket_id.to_owned(),
            process_epoch: EPOCH.to_owned(),
            domains: vec![(SyncDomain::Workers, DOMAIN_GENERATION, false)],
        },
    });
    core.handle(ClientEvent::HydrationCompleted { generation });
    core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::DomainReady {
            domain: SyncDomain::Workers,
            generation: DOMAIN_GENERATION,
            snapshot_token: None,
        },
    });
    assert!(core.store().sync.accepts(generation));
    generation
}

/// The recovery cursor, read the way a reconnect reads it: as the `since` the
/// next dial will send.
fn cursor_on_next_dial(core: &mut ClientCore) -> u64 {
    let effects = core.handle(ClientEvent::DialRequested);
    match effects.as_slice() {
        [Effect::DialSync { dial, .. }] => dial.since,
        other => panic!("expected exactly one dial, got {other:?}"),
    }
}

fn acks(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, Effect::SendSync(SyncCommand::Ack { .. })))
        .count()
}

fn enqueue(dispatch: &mut SyncDispatch, generation: u64, seq: u64, event_id: u64) {
    let frame = QueuedFrame::new(generation, seq, "sock-one", session_event(event_id));
    assert_eq!(dispatch.enqueue(frame), EnqueueOutcome::Queued);
}

#[test]
fn the_queue_bound_drops_the_oldest_and_says_so() {
    // Bounded at both ends: a queue at the coordinator's window size still fits
    // the frame that fills it, and the one past it evicts rather than grows.
    let mut dispatch = SyncDispatch::new();
    for seq in 1..=SYNC_DISPATCH_QUEUE_MAX as u64 {
        assert_eq!(
            dispatch.enqueue(QueuedFrame::new(1, seq, "sock-one", SyncFrame::Keepalive)),
            EnqueueOutcome::Queued,
            "a queue of {seq} frames must still fit"
        );
    }
    assert_eq!(dispatch.held(), SYNC_DISPATCH_QUEUE_MAX);
    let outcome = dispatch.enqueue(QueuedFrame::new(
        1,
        u64::MAX,
        "sock-one",
        SyncFrame::Keepalive,
    ));
    assert_eq!(outcome, EnqueueOutcome::QueuedEvictingOldest);
    assert_eq!(
        dispatch.held(),
        SYNC_DISPATCH_QUEUE_MAX,
        "the bound is a bound, not a suggestion"
    );
    let held = dispatch.take();
    assert_eq!(
        held[0].delivery_seq(),
        2,
        "the oldest went, and it was the oldest"
    );
    assert_eq!(held[held.len() - 1].delivery_seq(), u64::MAX);
}

fn take_one(dispatch: &mut SyncDispatch) -> QueuedFrame {
    let mut taken = dispatch.take();
    assert_eq!(taken.len(), 1, "expected exactly one held frame");
    taken.pop().expect("just asserted the length")
}

#[test]
fn a_frame_queued_across_a_reconnect_is_still_placeable() {
    let mut core = ClientCore::in_memory(TAB);
    let mut dispatch = SyncDispatch::new();
    let first = open_ready_link(&mut core, "sock-one");

    // An application frame arrives and is queued, not yet applied.
    enqueue(&mut dispatch, first, 7, 42);
    assert_eq!(dispatch.held(), 1);
    assert_eq!(
        dispatch.generations_held(),
        BTreeSet::from([first]),
        "the queue reports which generations it is still carrying"
    );

    // The socket dies, and a new generation opens in its place.
    core.handle(ClientEvent::SyncLinkClosed {
        generation: first,
        close_code: Some(1006),
    });
    assert!(!core.store().sync.accepts(first));
    let second = open_ready_link(&mut core, "sock-two");
    assert!(second > first, "a redial is a new generation");
    assert_eq!(core.store().sync.link_generation(), Some(second));
    assert_eq!(
        cursor_on_next_dial(&mut core),
        0,
        "nothing has been applied yet"
    );

    // The queued frame is still whole: its sequence, its socket, its generation.
    let frame = take_one(&mut dispatch);
    assert_eq!(frame.generation(), first);
    assert_eq!(frame.delivery_seq(), 7);
    assert_eq!(frame.socket_id(), "sock-one");
    assert!(frame.is_placeable());
    assert_eq!(frame.lane().domain, None);
    assert!(frame.lane().session_id.is_none());

    // And it applies. Dispatch is not generation gated: reconnecting cannot revoke
    // a frame that was accepted earlier, and the coordinator will not send it
    // again (`docs/phase4-client-contract.md` §7).
    let effects = core.handle(frame.into_event());
    let session = SessionId::try_from(SESSION).expect("a valid session id");
    assert!(
        core.store().sessions().session(&session).is_some(),
        "a frame accepted on a retired generation is still applied"
    );
    assert_eq!(
        acks(&effects),
        0,
        "the cumulative acknowledgement is gated to the socket that is current"
    );

    // The cursor advanced, and the NEXT dial resumes from it rather than from
    // zero — which is the whole point of keeping the delivery sequence.
    assert_eq!(cursor_on_next_dial(&mut core), 42);
}

#[test]
fn an_application_frame_with_no_delivery_sequence_is_refused_rather_than_queued() {
    // Coordinator contract §12.8, client side: a frame that reaches a queue
    // without its meta can never be placed, and the cursor stops there.
    let mut dispatch = SyncDispatch::new();
    let outcome = dispatch.enqueue(QueuedFrame::new(1, 0, "sock-one", session_event(42)));
    match outcome {
        EnqueueOutcome::Refused(refusal) => {
            assert_eq!(refusal.delivery_seq, 0);
            assert_eq!(refusal.generation, 1);
            assert_eq!(refusal.kind, "session_event");
            assert!(
                refusal.reason().contains("delivery sequence"),
                "the log has to name the value that was missing"
            );
        }
        other => panic!("an unsequenced application frame must be refused, got {other:?}"),
    }
    assert_eq!(dispatch.held(), 0, "a refused frame leaves nothing behind");

    // A control needs no sequence, so it is placeable either way: a control has
    // no window cost and is never acknowledged.
    assert_eq!(
        dispatch.enqueue(QueuedFrame::new(1, 0, "sock-one", SyncFrame::Keepalive)),
        EnqueueOutcome::Queued
    );
    assert_eq!(
        dispatch.enqueue(QueuedFrame::new(1, 9, "sock-one", SyncFrame::Keepalive)),
        EnqueueOutcome::Queued
    );
    assert_eq!(dispatch.held(), 2);
}

#[test]
fn the_queue_keeps_arrival_order_across_a_reconnect() {
    let mut dispatch = SyncDispatch::new();
    for (seq, event_id) in [(1_u64, 10_u64), (2, 11), (3, 12)] {
        enqueue(&mut dispatch, 1, seq, event_id);
    }
    let taken = dispatch.take();
    let sequences: Vec<u64> = taken.iter().map(QueuedFrame::delivery_seq).collect();
    assert_eq!(sequences, vec![1, 2, 3], "the coordinator sequenced these");
    assert!(dispatch.is_empty(), "a drain is a drain, not a peek");
}

#[test]
fn a_credential_boundary_drops_the_frames_keyed_to_it() {
    let mut core = ClientCore::in_memory(TAB);
    let mut dispatch = SyncDispatch::new();
    let generation = open_ready_link(&mut core, "sock-one");
    enqueue(&mut dispatch, generation, 1, 10);
    enqueue(&mut dispatch, generation, 2, 11);

    // One is applied, so there is a cursor that a persisted global one would skip
    // the next socket's history past.
    let applied = take_one(&mut dispatch);
    core.handle(applied.into_event());
    assert_eq!(cursor_on_next_dial(&mut core), 10);

    core.handle(ClientEvent::CredentialsDiscarded);
    assert_eq!(dispatch.clear(), 1, "the frame still held went with it");
    assert!(dispatch.is_empty());
    // The cursor went with the credential, so the next socket's initial history
    // is not skipped (`docs/phase4-client-contract.md` §7).
    assert_eq!(cursor_on_next_dial(&mut core), 0);
}

#[test]
fn state_from_a_previous_generation_is_refused_after_a_new_one_opens() {
    let mut core = ClientCore::in_memory(TAB);
    let first = open_ready_link(&mut core, "sock-one");
    core.handle(ClientEvent::SyncLinkClosed {
        generation: first,
        close_code: Some(1006),
    });
    let second = open_ready_link(&mut core, "sock-two");
    assert_ne!(first, second);

    // A close naming the OLD generation is not this socket's close. If it were
    // honoured, a credential verdict meant for a socket nobody is using would
    // stop the dial loop.
    let before = core.store().revision();
    let effects = core.handle(ClientEvent::SyncLinkClosed {
        generation: first,
        close_code: Some(4001),
    });
    assert!(effects.is_empty());
    assert_eq!(core.store().revision(), before, "nothing was mutated");
    assert!(
        !core.store().sync.auth_revoked,
        "a verdict for a retired socket must not stop this one"
    );
    assert!(core.store().sync.accepts(second));
    assert_eq!(core.store().sync.socket_id(), Some("sock-two"));
    assert_eq!(core.store().sync.link_generation(), Some(second));
}

#[test]
fn a_socket_from_a_replaced_generation_is_neither_adopted_nor_read() {
    let mut link = InstalledLink::empty();
    link.dialled(1);
    assert!(link.opened(1));
    assert!(can_open_sync_link(&link, 1));
    assert!(can_accept_sync_link(&link, 1));

    // The host replaces the socket. The old handle can still deliver into its
    // callbacks, and adopting it would install a generation the store has already
    // retired.
    link.dialled(2);
    assert!(!link.opened(1), "a replaced socket must not be adopted");
    assert!(!can_accept_sync_link(&link, 1));
    assert!(
        !link.retire(1, AbortReason::Manual),
        "a replaced link is not retired a second time"
    );
    assert_eq!(link.abort_reason(), None);
    assert!(link.opened(2));
    assert!(can_accept_sync_link(&link, 2));
    assert_eq!(link.generation(), Some(2));

    // A link retired for a reason is not adoptable, and stops accepting BEFORE it
    // is closed — a frame between the two states would be applied to a
    // generation this side has already given up.
    link.retire(2, AbortReason::TerminalLiveness);
    assert!(!can_open_sync_link(&link, 2));
    assert!(!can_accept_sync_link(&link, 2));
    assert!(
        link.is_open(),
        "retiring stops accepting; it does not close"
    );
    assert_eq!(link.abort_reason(), Some(AbortReason::TerminalLiveness));
    link.closed(2);
    assert!(!link.is_open());
    assert_eq!(
        link.abort_reason(),
        Some(AbortReason::TerminalLiveness),
        "a close does not erase the reason the link was retired for"
    );
}

#[test]
fn a_revoked_credential_stops_the_dial_loop() {
    let mut core = ClientCore::in_memory(TAB);
    let generation = open_ready_link(&mut core, "sock-one");

    core.handle(ClientEvent::SyncLinkClosed {
        generation,
        close_code: Some(4001),
    });
    assert!(core.store().sync.auth_revoked);

    let effects = core.handle(ClientEvent::DialRequested);
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::DialSync { .. })),
        "a revoked credential must not be presented again"
    );
}

#[test]
fn a_backpressure_close_redials_at_once_and_keeps_the_cursor() {
    let mut core = ClientCore::in_memory(TAB);
    let mut dispatch = SyncDispatch::new();
    let generation = open_ready_link(&mut core, "sock-one");
    enqueue(&mut dispatch, generation, 4, 77);
    // Apply it, so there is a cursor worth resuming from.
    let applied = take_one(&mut dispatch);
    let effects = core.handle(applied.into_event());
    assert_eq!(acks(&effects), 1, "the current socket is acknowledged");

    let close = classify_close(Some(1013), "sync backpressure");
    assert!(close.redials_immediately());
    assert!(close.preserved_records());
    core.handle(ClientEvent::SyncLinkClosed {
        generation,
        close_code: Some(1013),
    });

    // The next dial resumes at the cursor rather than re-hydrating, and the
    // records the coordinator was still holding come back on the new socket.
    assert_eq!(cursor_on_next_dial(&mut core), 77);
}
