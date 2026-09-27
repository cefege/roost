// A shared file and two events, for the two durable-outbox suites that need
// them: the rows' survival and the claims' survival. An integration test is its
// own crate and `expect` is denied outside `#[cfg(test)]`, so the exemption
// lives here rather than in every test that builds a fixture.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![allow(dead_code)]

//! One scratch outbox file per test, and the two events every durable-outbox
//! test writes: an `opened` and a `closed`.
//!
//! It is a shared file because a store reopened over the SAME file is the whole
//! point of half these tests, so a path per test is a path no two tests share —
//! and it is a `Scratch` rather than a tempdir crate because a store that
//! outlives the process that opened it has to outlive the fixture that names it.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use roost_protocol::wire::brand::{ChannelId, SessionId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::SessionKind;
use roost_worker::event_store::{DATABASE_FILE_NAME, Journal};

/// One directory, removed when the value goes out of scope.
pub struct Scratch {
    root: PathBuf,
}

impl Scratch {
    /// A directory of this test's own, named for what the test is proving so a
    /// leftover from a failed run says which property was being checked.
    pub fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "roost-outbox-{label}-{}-{ordinal}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).unwrap_or_else(|error| {
            panic!("the scratch root {} is unusable: {error}", root.display())
        });
        Self { root }
    }

    /// The outbox file inside it, which is what a second process reopens.
    pub fn file(&self) -> PathBuf {
        self.root.join(DATABASE_FILE_NAME)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A test that failed mid-write leaves a store behind, and the cleanup is
        // best effort because a failure here must not mask the assertion that
        // already failed.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// A store over `scratch`'s file, opened the way the worker opens it.
pub async fn journal_in(scratch: &Scratch) -> Journal {
    Journal::open(&scratch.file())
        .await
        .expect("a fresh outbox opens")
}

pub const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";
pub const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
pub const OTHER: &str = "00000000-0000-4000-8000-00000000cafe";

/// The event a session begins with, and the one it ends with.
pub fn opened(session: &str) -> SessionEvent {
    SessionEvent::Opened {
        session_id: SessionId::try_from(session).expect("a uuid is a session id"),
        worker_fp: WorkerFp::try_from(FINGERPRINT).expect("64 hex characters is a fingerprint"),
        channel: ChannelId::try_from(1_i64).expect("a small channel id"),
        session_kind: SessionKind::Shell,
        cwd: "/home/user/project".to_string(),
        ts: 1_700_000_000_000,
        trace_id: None,
    }
}

/// The other end of the same session: the row a claim exists to make room for.
pub fn closed(session: &str) -> SessionEvent {
    SessionEvent::Closed {
        session_id: SessionId::try_from(session).expect("a uuid is a session id"),
        exit_code: Some(0),
        ts: 1_700_000_001_000,
        trace_id: None,
    }
}
