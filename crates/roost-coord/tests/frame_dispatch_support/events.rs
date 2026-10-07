//! The `SessionEvent`s the dispatcher tests speak in, built once.
//!
//! Split from the fixture so three test binaries can share them without any one
//! of them carrying the builders the other two do not use. Every builder is a
//! function rather than a constant because a `SessionEvent` owns branded ids and
//! a brand is checked at construction, not at use.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

use roost_protocol::wire::{ChannelId, SessionEvent, SessionKind};

use super::{SESSION_ID, WORKER_FP, session_id, worker};

/// The first event of every worker's outbox.
#[must_use]
pub fn opened(worker_fp: &str, channel: i64) -> SessionEvent {
    SessionEvent::Opened {
        session_id: session_id(),
        worker_fp: worker(worker_fp),
        channel: channel_id(channel),
        session_kind: SessionKind::Shell,
        cwd: "/tmp".to_owned(),
        ts: 1_000,
        trace_id: None,
    }
}

/// The end of a session this worker opened.
#[must_use]
pub fn closed() -> SessionEvent {
    SessionEvent::Closed {
        session_id: session_id(),
        exit_code: Some(0),
        ts: 1_100,
        trace_id: None,
    }
}

/// A keeper reboot behind a session, on a fresh channel.
#[must_use]
pub fn respawned(channel: i64) -> SessionEvent {
    SessionEvent::Respawned {
        session_id: session_id(),
        new_channel: channel_id(channel),
        ts: 1_200,
        trace_id: None,
    }
}

/// A viewer attaching. **Not** on the pre-barrier allow list, which is what makes
/// it the fixture's refused-by-the-transport-gate case.
#[must_use]
pub fn attached() -> SessionEvent {
    SessionEvent::Attached {
        session_id: session_id(),
        ts: 1_400,
        trace_id: None,
    }
}

/// A worker's whole live set on reconnect.
#[must_use]
pub fn snapshot(sessions: Vec<roost_protocol::wire::Session>) -> SessionEvent {
    SessionEvent::Snapshot {
        worker_fp: worker(WORKER_FP),
        sessions,
        ts: 1_500,
        trace_id: None,
    }
}

/// A channel the brand accepts.
fn channel_id(channel: i64) -> ChannelId {
    ChannelId::try_from(channel).expect("the fixture channel fits")
}

/// The fixture's session id, for an assertion that names it.
pub const SESSION: &str = SESSION_ID;
