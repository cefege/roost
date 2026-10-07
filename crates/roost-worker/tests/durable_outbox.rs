//! The durable session-event outbox's ROWS, at the store level. The property
//! throughout is the one the file exists for: **an event the coordinator never
//! acknowledged is still there after a restart**, and nothing else removes it.
//!
//! The other two properties of the same outbox live beside this one, and were
//! in this file until the suite outgrew a single reading: the room a session
//! claims for its close, and the lease that hands it back, are in
//! `durable_outbox_claims.rs`; the volatile producers' fold is in
//! `durable_outbox_coalescing.rs`.
//!
//! The link-level half of the same rule — that `opened` reaches the coordinator
//! before that session's first cells — lives beside the code that enforces it, in
//! `runtime::link_loop::durable`'s own tests, because it needs the barrier and
//! the drain and neither is reachable from here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "outbox_support/mod.rs"]
mod support;

use support::{OTHER, SESSION, Scratch, closed, journal_in, opened};

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
        journal
            .append(&opened(SESSION))
            .await
            .expect("appended")
            .client_seq
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

    assert!(
        !journal.acknowledge(0).await.expect("ack"),
        "zero is not a sequence"
    );
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
        expected.push(
            journal
                .append(&opened(session))
                .await
                .expect("appended")
                .client_seq,
        );
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

/// A pending row of a kind an earlier build wrote and this build cannot decode
/// (an OMP conversation reference) is discarded at open; the rows around it
/// stay and the outbox opens rather than refusing to start the worker.
#[tokio::test]
async fn a_pending_row_of_a_retired_kind_is_discarded_at_open() {
    let scratch = Scratch::new("retired-kind");
    {
        let journal = journal_in(&scratch).await;
        journal.append(&opened(SESSION)).await.expect("appended");
        journal.append(&closed(SESSION)).await.expect("appended");
    }
    let reference =
        format!(r#"{{"kind":"agent_reference","session_id":"{SESSION}","reference":null,"ts":5}}"#);
    let pool = sqlx::sqlite::SqlitePool::connect(&format!("sqlite://{}", scratch.file().display()))
        .await
        .expect("the file opens");
    sqlx::query(
        "UPDATE session_events SET kind = 'agent_reference', event_json = $1, payload_bytes = $2 \
         WHERE client_seq = 2",
    )
    .bind(&reference)
    .bind(i64::try_from(reference.len()).unwrap())
    .execute(&pool)
    .await
    .expect("the row is rewritten");
    pool.close().await;

    let journal = journal_in(&scratch).await;
    let pending = journal.pending().await.expect("read");
    assert_eq!(pending.len(), 1, "only the decodable row is left");
    assert_eq!(pending[0].client_seq, 1);
}
