//! The terminal commands the core emits, as the coordinator receives them: a
//! view's revision moves on every intent change and holds across renewals (v2
//! `terminal-stream-view-commands.ts:139-165`), and a resync names the replica's
//! expected stream and canonical position, or is not sent at all
//! (`terminal-stream-repair.ts:221-235`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use roost_client_core::client::sync::encode_sync_command;
use roost_client_core::{ClientCore, ClientEvent, Effect, SyncCommand};
use roost_proto::__buffa::oneof::sync_client_frame::Command;
use roost_proto::buffa::Message;
use roost_proto::{SyncClientFrame, TerminalResyncCommand, TerminalViewCommand};
use roost_protocol::viewport::TERMINAL_VIEW_HEARTBEAT_MS;
use support::{EPOCH, SESSION, STREAM, client_with_clock, delta, full, sync_token};

const VIEW: &str = "view-1";

fn wire(effects: &[Effect]) -> Vec<Command> {
    effects
        .iter()
        .filter_map(|effect| match effect {
            Effect::SendSync(command) => {
                let bytes = encode_sync_command(command, "socket-1");
                SyncClientFrame::decode_from_slice(&bytes).unwrap().command
            }
            _ => None,
        })
        .collect()
}

fn views(effects: &[Effect]) -> Vec<TerminalViewCommand> {
    wire(effects)
        .into_iter()
        .filter_map(|command| match command {
            Command::TerminalView(view) => Some(*view),
            _ => None,
        })
        .collect()
}

fn resyncs(effects: &[Effect]) -> Vec<TerminalResyncCommand> {
    wire(effects)
        .into_iter()
        .filter_map(|command| match command {
            Command::TerminalResync(resync) => Some(*resync),
            _ => None,
        })
        .collect()
}

/// `(revision, active, cols, rows)` of the one view command an event produced.
fn only_view(effects: &[Effect]) -> (u64, bool, u32, u32) {
    let sent = views(effects);
    assert_eq!(
        sent.len(),
        1,
        "expected exactly one view command, got {sent:?}"
    );
    (sent[0].revision, sent[0].active, sent[0].cols, sent[0].rows)
}

fn bound_core() -> (ClientCore, std::rc::Rc<roost_client_core::MemoryClock>) {
    let (mut core, clock) = client_with_clock();
    core.store_mut()
        .terminal_mut(SESSION, "fp-1")
        .bind_generation(&sync_token(1, 1));
    (core, clock)
}

fn open(core: &mut ClientCore, cols: u32, rows: u32) -> Vec<Effect> {
    core.handle(ClientEvent::ViewOpened {
        session_id: SESSION.to_owned(),
        worker_fp: "fp-1".to_owned(),
        view_id: VIEW.to_owned(),
        cols,
        rows,
    })
}

#[test]
fn a_view_revision_moves_on_each_new_intent_and_holds_on_renewal() {
    let (mut core, clock) = bound_core();
    assert_eq!(only_view(&open(&mut core, 80, 24)), (1, true, 80, 24));

    let resized = core.handle(ClientEvent::ViewResized {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
        cols: 100,
        rows: 30,
    });
    assert_eq!(only_view(&resized), (2, true, 100, 30));

    // A heartbeat renews the SAME intent: same revision, same payload, which
    // the authority treats as idempotent rather than as a new claim.
    clock.set(TERMINAL_VIEW_HEARTBEAT_MS + 1);
    let renewed = core.handle(ClientEvent::Sweep {
        now_ms: TERMINAL_VIEW_HEARTBEAT_MS + 1,
    });
    assert_eq!(only_view(&renewed), (2, true, 100, 30));

    let hide = || ClientEvent::ViewHidden {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    };
    assert_eq!(only_view(&core.handle(hide())), (3, false, 0, 0));
    // Hiding a hidden view is not a new intent.
    assert_eq!(only_view(&core.handle(hide())), (3, false, 0, 0));

    let closed = core.handle(ClientEvent::ViewClosed {
        session_id: SESSION.to_owned(),
        view_id: VIEW.to_owned(),
    });
    assert_eq!(only_view(&closed), (4, false, 0, 0));
}

#[test]
fn reopening_a_held_view_continues_its_revision() {
    let (mut core, _clock) = bound_core();
    assert_eq!(only_view(&open(&mut core, 80, 24)).0, 1);
    // Starting again at 1 would read as a stale revision at the authority.
    assert_eq!(only_view(&open(&mut core, 90, 24)).0, 2);
}

#[test]
fn a_resync_names_the_expected_stream_and_the_canonical_position() {
    let (mut core, clock) = bound_core();
    let token = sync_token(1, 1);
    {
        let replica = core.store_mut().terminal_mut(SESSION, "fp-1");
        replica.install_expected_stream(STREAM, 8, 4);
        replica.admit_frame(&full(4), false, &token, 0);
    }
    let _ = open(&mut core, 8, 4);
    // A delta that does not continue seq 1 is a gap: the replica latches.
    let replica = core.store_mut().terminal_mut(SESSION, "fp-1");
    replica.admit_frame(&delta(5, 4, 0), false, &token, 0);
    assert!(
        replica.repair_latched(),
        "the fixture gap must latch a repair"
    );

    clock.set(1);
    let sent = resyncs(&core.handle(ClientEvent::Sweep { now_ms: 1 }));
    assert_eq!(sent.len(), 1, "a latched replica asks for one baseline");
    assert_eq!(sent[0].stream_id, STREAM);
    assert_eq!(sent[0].grid_epoch, EPOCH);
    assert_eq!(sent[0].seq, 1, "the canonical full the replica holds");
    assert_eq!(sent[0].view_id, VIEW);
    assert_eq!(sent[0].domain_generation, 1);
}

#[test]
fn a_replica_expecting_no_stream_sends_no_resync() {
    let (mut core, clock) = bound_core();
    let _ = open(&mut core, 8, 4);
    let replica = core.store_mut().terminal_mut(SESSION, "fp-1");
    // A frame naming another session is refused and latches even before any
    // stream is expected.
    let mut foreign = full(4);
    foreign.session_id = "00000000-0000-4000-8000-0000000000ff".to_owned();
    replica.admit_frame(&foreign, false, &sync_token(1, 1), 0);
    assert!(
        replica.repair_latched(),
        "the fixture refusal must latch a repair"
    );
    assert_eq!(replica.expected_stream_id(), None);

    clock.set(1);
    let effects = core.handle(ClientEvent::Sweep { now_ms: 1 });
    assert!(
        !effects
            .iter()
            .any(|effect| matches!(effect, Effect::SendSync(SyncCommand::TerminalResync { .. }))),
        "a resync with no stream to be a baseline of was sent: {effects:?}"
    );
}
