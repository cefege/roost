//! The durable session-event outbox, at the store level. The property
//! throughout is the one the file exists for: **an event the coordinator never
//! acknowledged is still there after a restart**, and nothing else removes it.
//!
//! The link-level half of the same rule — that `opened` reaches the coordinator
//! before that session's first cells — lives beside the code that enforces it, in
//! `runtime::link_loop::durable`'s own tests, because it needs the barrier and
//! the drain and neither is reachable from here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use roost_protocol::wire::brand::{ChannelId, SessionId, WorkerFp};
use roost_protocol::wire::event::SessionEvent;
use roost_protocol::wire::session::SessionKind;
use roost_worker::event_store::{DATABASE_FILE_NAME, DurableEventKind, Journal};
use roost_worker::outbox::{Admitted, Lane, Outbox};

/// One directory, removed when the value goes out of scope.
///
/// A store that is reopened over the SAME file is the whole point of one of
/// these tests, so a path per test is a path no two tests share.
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(label: &str) -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "roost-outbox-{label}-{}-{ordinal}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root)
            .unwrap_or_else(|error| panic!("the scratch root {} is unusable: {error}", root.display()));
        Self { root }
    }

    fn file(&self) -> PathBuf {
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

const FINGERPRINT: &str = "000000000000000000000000000000000000000000000000000000000000f00d";
const SESSION: &str = "00000000-0000-4000-8000-00000000beef";
const OTHER: &str = "00000000-0000-4000-8000-00000000cafe";

fn opened(session: &str) -> SessionEvent {
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

fn closed(session: &str) -> SessionEvent {
    SessionEvent::Closed {
        session_id: SessionId::try_from(session).expect("a uuid is a session id"),
        exit_code: Some(0),
        ts: 1_700_000_001_000,
        trace_id: None,
    }
}

async fn journal_in(scratch: &Scratch) -> Journal {
    Journal::open(&scratch.file())
        .await
        .expect("a fresh outbox opens")
}

/// THE NAMED PROPERTY. A worker that wrote an event and never saw the
/// coordinator acknowledge it must offer that event again after a restart —
/// otherwise the coordinator's record of what happened has a silent hole, and
/// the session a browser is watching was opened by a worker nobody told.
#[tokio::test]
async fn an_un_acknowledged_row_survives_a_restart() {
    let scratch = Scratch::new("survive");
    let first_sequence = {
        let journal = journal_in(&scratch).await;
        let row = journal.append(&opened(SESSION)).await.expect("appended");
        assert_eq!(row.client_seq, 1);
        assert_eq!(journal.pending().await.expect("read").len(), 1);
        row.client_seq
    };

    // A second process over the same file, which is what a restart is.
    let journal = journal_in(&scratch).await;
    let head = journal
        .replay_head()
        .await
        .expect("read")
        .expect("the row the coordinator never acknowledged is still waiting");
    assert_eq!(
        head.client_seq, first_sequence,
        "a restart renumbered the row, so the coordinator would read the replay as a new event"
    );
    assert_eq!(
        head.event,
        opened(SESSION),
        "the replay is a different event from the one that was written"
    );
    assert_eq!(head.kind, "opened");
}

/// A number this file already burned is never handed out again, and the barrier
/// resumes above it. A repeat would let a replayed event be mistaken for a new
/// one, which is the one sequence defect a durable store cannot recover from.
#[tokio::test]
async fn a_sequence_is_never_handed_out_twice_across_a_restart() {
    let scratch = Scratch::new("sequence");
    let first = {
        let journal = journal_in(&scratch).await;
        journal.append(&opened(SESSION)).await.expect("appended").client_seq
    };
    let next = {
        let journal = journal_in(&scratch).await;
        assert_eq!(
            journal.handed_over_at(),
            first,
            "the restarted store did not read the sequence already used"
        );
        let next = journal.append(&closed(SESSION)).await.expect("appended");
        assert!(
            next.client_seq > first,
            "the restarted store handed out {first} again, so the coordinator could not tell the \
             close from the open"
        );
        next
    };
    let journal = journal_in(&scratch).await;
    let waiting = journal.pending().await.expect("read");
    // BOTH rows, and that is the point: the second process wrote a close and
    // was never acknowledged for it, so a close it has no answer for is still
    // waiting. Only an acknowledgement retires a row, so "the process ended" is
    // not one.
    assert_eq!(
        waiting.iter().map(|row| row.client_seq).collect::<Vec<_>>(),
        vec![first, next.client_seq],
        "a row the coordinator never acknowledged did not survive the restart"
    );
}

/// A row leaves on ITS sequence and no other. Acknowledging a sequence no row
/// holds is a stale or duplicated answer and must retire nothing, because the
/// row that IS waiting is one the coordinator has not confirmed.
#[tokio::test]
async fn a_row_leaves_only_on_its_exact_acknowledgement() {
    let scratch = Scratch::new("exact");
    let journal = journal_in(&scratch).await;
    let first = journal.append(&opened(SESSION)).await.expect("appended");
    let second = journal.append(&closed(OTHER)).await.expect("appended");

    assert!(!journal.acknowledge(0).await.expect("ack"), "zero is not a sequence");
    assert!(
        !journal.acknowledge(9_999).await.expect("ack"),
        "a sequence no row holds retired something"
    );
    assert_eq!(journal.pending().await.expect("read").len(), 2);

    assert!(
        journal.acknowledge(second.client_seq).await.expect("ack"),
        "the acknowledged row did not leave"
    );
    let waiting = journal.pending().await.expect("read");
    assert_eq!(
        waiting.iter().map(|row| row.client_seq).collect::<Vec<_>>(),
        vec![first.client_seq],
        "acknowledging the close also retired the open, and the open is the row the coordinator \
         has still never confirmed"
    );
    assert!(
        !journal.acknowledge(second.client_seq).await.expect("ack"),
        "the same acknowledgement retired the row twice"
    );
}

/// ONE row at a time, oldest first, for ever. Two rows in flight would make the
/// coordinator's two acknowledgements ambiguous, and an ambiguous
/// acknowledgement is the state a durable path cannot recover from on its own.
#[tokio::test]
async fn the_replay_head_is_one_row_and_always_the_oldest() {
    let scratch = Scratch::new("head");
    let journal = journal_in(&scratch).await;
    let mut expected = Vec::new();
    for session in [SESSION, OTHER, SESSION] {
        expected.push(journal.append(&opened(session)).await.expect("appended").client_seq);
    }
    for want in expected {
        let head = journal
            .replay_head()
            .await
            .expect("read")
            .expect("a row is waiting");
        assert_eq!(head.client_seq, want, "the head skipped or repeated a row");
        assert!(
            journal.acknowledge(want).await.expect("ack"),
            "the head did not retire under its own acknowledgement"
        );
    }
    assert!(
        journal.replay_head().await.expect("read").is_none(),
        "an empty outbox still offered a row"
    );
}

/// The volatile producers' fold: one record per key, replaced in place. A link
/// applying backpressure must not accumulate every version of one agent status
/// and then ship them all in order, because the coordinator would walk a
/// replacement edge it has already passed.
#[test]
fn a_coalescing_frame_replaces_its_own_predecessor_in_one_lane() {
    let mut outbox = Outbox::default();
    let now = std::time::Instant::now();
    assert_eq!(
        outbox.admit_coalescing("s-1", Lane::Control, vec![0; 8], "first", now),
        Ok(Admitted::Queued)
    );
    assert_eq!(
        outbox.admit_coalescing("s-1", Lane::Control, vec![1; 8], "second", now),
        Ok(Admitted::Coalesced)
    );
    assert_eq!(
        outbox.admit_coalescing("s-2", Lane::Control, vec![2; 8], "other", now),
        Ok(Admitted::Queued)
    );

    assert_eq!(outbox.frame_count(), 2, "one record per key, not one per version");
    assert!(outbox.coalesces(Lane::Control, "s-1"));
    assert!(outbox.coalesces(Lane::Control, "s-2"));

    let drained = outbox.drain_all(now);
    assert_eq!(drained.len(), 2);
    assert_eq!(drained[0].bytes, vec![1; 8], "the replaced version is the one that went");
    assert_eq!(drained[0].label, "second");
    assert_eq!(drained[1].bytes, vec![2; 8], "a different key's record is untouched");
}

/// A frame that does not fit beside the one it would replace is refused, and the
/// older record STAYS. Refusing is the only truthful answer: a title the
/// coordinator has already been told is stale, but a refusal is a refusal.
#[test]
fn a_coalescing_frame_that_does_not_fit_keeps_the_record_it_would_replace() {
    let mut outbox = Outbox::new(8, 16);
    let now = std::time::Instant::now();
    // A second key fills the cap, so the replacement has to be judged against a
    // queue that is already full rather than against an empty one.
    outbox
        .admit_coalescing("s-0", Lane::Control, vec![9; 8], "other", now)
        .expect("admitted");
    outbox
        .admit_coalescing("s-1", Lane::Control, vec![0; 8], "first", now)
        .expect("admitted");
    let refused = outbox.admit_coalescing("s-1", Lane::Control, vec![1; 12], "second", now);
    assert!(
        refused.is_err(),
        "a 12-byte record replaced 8 bytes in a 16-byte queue that already held 8"
    );
    assert_eq!(outbox.byte_count(), 16, "the refused record changed the byte count");
    let drained = outbox.drain_all(now);
    assert_eq!(drained[1].label, "first", "the record that was told is the one that left");
}

// ---------------------------------------------------------------------------
// A claim is PERSISTED, and the lease is what makes persistence safe.
// ---------------------------------------------------------------------------

/// THE POINT OF PERSISTING IT. A session claims the room its `closed` event will
/// need BEFORE the PTY exists, so a spawn that cannot record its end never opens
/// one. A claim held only in a heap is silently gone after a reboot, and the
/// store then admits an `opened` it has no room to close.
#[tokio::test]
async fn a_claim_survives_a_close_and_reopen() {
    let scratch = Scratch::new("claim-survives");
    let first = {
        let journal = journal_in(&scratch).await;
        let claim = journal
            .reserve(DurableEventKind::Closed, 2 * 1024)
            .await
            .expect("a fresh outbox has room");
        (claim.id(), claim.kind())
    };
    let journal = journal_in(&scratch).await;
    assert_eq!(
        journal.claims().await.expect("read"),
        (1, 1),
        "the claim was gone, so a session that had taken room for its close no longer holds any"
    );
    // A second claim must not reuse the first one's id: a claim that came back
    // with an identity already in use is a claim the first session and the
    // second both own, which is the failure the token exists to prevent.
    let second = journal
        .reserve(DurableEventKind::Opened, 2 * 1024)
        .await
        .expect("room remains");
    assert_ne!(second.id(), first.0, "a claim id was handed out twice");
    assert_eq!(second.kind(), DurableEventKind::Opened);
    assert_eq!(first.1, DurableEventKind::Closed);
    let held = journal.hold(second).await.expect("held");
    journal.release(held).await.expect("released");
    assert_eq!(journal.claims().await.expect("read"), (1, 1), "the released claim is still there");
}

/// THE LEASE. A claim from a process that died is capacity nobody hands back,
/// and in a FILE it would never be handed back at all — so the store reclaims
/// the ones past the lease, and only those.
#[tokio::test]
async fn a_claim_past_the_lease_is_reclaimed_and_one_inside_it_is_not() {
    let scratch = Scratch::new("claim-lease");
    let journal = journal_in(&scratch).await;
    let stale = journal
        .reserve(DurableEventKind::Closed, 2 * 1024)
        .await
        .expect("a fresh outbox has room");
    let fresh = journal
        .reserve(DurableEventKind::Opened, 2 * 1024)
        .await
        .expect("room remains");

    // The store's OWN clock, which is a hair after the two reserves and
    // therefore well inside the lease. Nothing goes: a sweep that took a live
    // spawn's claim would leave a session mid-spawn with no room for its close,
    // and no error anywhere to say so.
    assert_eq!(
        journal.reclaim_expired_claims(now_ms()).await.expect("swept"),
        0,
        "a live spawn's claim was reclaimed, so a session mid-spawn lost the room it had already \
         taken for its close"
    );
    assert_eq!(journal.claims().await.expect("read"), (2, 2));

    // A clock far enough ahead that both claims are unambiguously past it. Both
    // go, and the store is empty — which is the failure the lease exists to
    // prevent: capacity held for ever by processes that are gone.
    let reclaimed = journal
        .reclaim_expired_claims(i64::MAX / 2)
        .await
        .expect("swept");
    assert_eq!(reclaimed, 2, "two claims past any plausible lease survived the sweep");
    assert_eq!(journal.claims().await.expect("read"), (0, 0));
    let _ = (stale, fresh);
}

/// The store's own clock. The reclaim takes the clock as an ARGUMENT precisely
/// so this test is expressible: one that cannot move time cannot tell a stale
/// claim from a fresh one, and a rule that cannot is indistinguishable from one
/// that discards everything.
fn now_ms() -> i64 {
    roost_worker::event_store::database::claims::now_ms()
}

/// `emit` is the primitive: the row and the retirement of its claim are ONE
/// transaction, because two would leave a row on disk with a live claim behind
/// it — and that is the ORDINARY path, not a crash, so it would leak on every
/// single event rather than rarely.
#[tokio::test]
async fn emitting_an_event_spends_its_claim_in_the_same_transaction() {
    let scratch = Scratch::new("claim-emit");
    let journal = journal_in(&scratch).await;
    let claim = journal
        .reserve(DurableEventKind::Opened, 512)
        .await
        .expect("a fresh outbox has room");
    assert_eq!(journal.claims().await.expect("read"), (1, 1));

    let row = journal
        .emit(claim, &opened(SESSION))
        .await
        .expect("emitted");
    assert_eq!(row.kind, "opened");
    assert_eq!(
        journal.claims().await.expect("read"),
        (0, 0),
        "the row is on disk and the claim is still held, so the same capacity is counted twice"
    );
    assert_eq!(journal.pending().await.expect("read").len(), 1);
    journal.acknowledge(row.client_seq).await.expect("acked");
    assert!(journal.pending().await.expect("read").is_empty());
}
