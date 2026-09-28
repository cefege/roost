//! The fixtures the two Sync reconnect suites share: one synthetic session
//! event, a link taken all the way to a ready domain, and the recovery cursor
//! read the way a reconnect reads it.
//!
//! Owned here because both halves of the reconnect contract need the same
//! handshake, and a second copy of it is a second answer to "what does a ready
//! link look like". Mirrors `crates/roost-client-core/tests/support/auth.rs`.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use roost_client_core::client::sync::{EnqueueOutcome, QueuedFrame, SyncDispatch};
use roost_client_core::effect::{Effect, SyncCommand};
use roost_client_core::event::ClientEvent;
use roost_client_core::{ClientCore, SyncDomain, SyncFrame, WireEvent};
use roost_protocol::wire::{ChannelId, SessionEvent, SessionId, SessionKind, WorkerFp};

pub const TAB: &str = "tab-7f3a";
pub const EPOCH: &str = "epoch-1";
pub const SESSION: &str = "00000000-0000-4000-8000-00000000000a";
pub const WORKER_FP: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
/// The domain generation every fixture link announces.
pub const DOMAIN_GENERATION: u64 = 1;

pub fn session_event(event_id: u64) -> SyncFrame {
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

/// Dial, complete the handshake, announce the terminal and workers domains,
/// answer their hydration calls, and so close both snapshot/live gaps. Returns the generation the socket took.
///
/// A second call replaces the first socket's link, so a caller that wants the old
/// one still open has to say so itself: `SyncState::open_link` refuses to install
/// a second live link, which is what stops two sockets being current at once.
pub fn open_ready_link(core: &mut ClientCore, socket_id: &str) -> u64 {
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
    // The terminal domain is announced with the workers domain because the
    // store holds application frames until the protected sessions snapshot
    // publishes, exactly as v2 publishes nothing before it. `true` is the
    // coordinator stating that THIS client is subscribed on THIS socket
    // (`sync.proto:115-125`); only a subscribed domain is hydrated.
    let effects = core.handle(ClientEvent::SyncFrameReceived {
        generation,
        delivery_seq: 0,
        frame: SyncFrame::Subscribed {
            socket_id: socket_id.to_owned(),
            process_epoch: EPOCH.to_owned(),
            domains: vec![
                (SyncDomain::Terminal, DOMAIN_GENERATION, true),
                (SyncDomain::Workers, DOMAIN_GENERATION, true),
            ],
        },
    });
    super::hydration::answer_hydrations(core, &effects);
    assert!(core.store().sync.domain_is_ready(SyncDomain::Workers));
    assert!(core.store().sync.domain_is_ready(SyncDomain::Terminal));
    assert!(core.store().sync.accepts(generation));
    generation
}

/// The recovery cursor, read the way a reconnect reads it: as the `since` the
/// next dial will send.
pub fn cursor_on_next_dial(core: &mut ClientCore) -> u64 {
    let effects = core.handle(ClientEvent::DialRequested);
    match effects.as_slice() {
        [Effect::DialSync { dial, .. }] => dial.since,
        other => panic!("expected exactly one dial, got {other:?}"),
    }
}

pub fn acks(effects: &[Effect]) -> usize {
    effects
        .iter()
        .filter(|effect| matches!(effect, Effect::SendSync(SyncCommand::Ack { .. })))
        .count()
}

pub fn enqueue(dispatch: &mut SyncDispatch, generation: u64, seq: u64, event_id: u64) {
    let frame = QueuedFrame::new(generation, seq, "sock-one", session_event(event_id));
    assert_eq!(dispatch.enqueue(frame), EnqueueOutcome::Queued);
}
