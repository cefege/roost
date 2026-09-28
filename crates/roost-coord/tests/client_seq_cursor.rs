//! One allocator for one sequence space, and the three verdicts it answers with.
//!
//! Every case here is a state the coordinator cannot distinguish from a lost
//! frame if the cursor is wrong, which is why each is named for the state it
//! prevents rather than for the code it exercises.
//!
//! `expect` and `unwrap` are denied outside `#[cfg(test)]`, and an integration
//! test is its own crate rather than a module of one, so the exemption has to
//! be stated here rather than inherited.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use roost_coord::worker_link::client_seq::{ClientSeqCursor, ClientSeqCursors, SeqVerdict};

#[tokio::test]
async fn the_first_frame_is_sequence_one_and_the_next_is_its_successor() {
    let cursor = ClientSeqCursor::new();
    assert_eq!(cursor.offer(1).await, SeqVerdict::Admit);
    assert_eq!(cursor.offer(2).await, SeqVerdict::Admit);
    assert_eq!(cursor.last_admitted().await, Some(2));
}

#[tokio::test]
async fn a_retry_is_a_dedupe_and_does_not_move_the_cursor() {
    let cursor = ClientSeqCursor::new();
    cursor.offer(1).await;
    cursor.offer(2).await;

    // The worker resent sequence 2 because it never saw the ACK. It is DEDUPED
    // and ACKed again — and if the cursor advanced here it would leave a gap at
    // 3, so every later frame would read as out of order against a sequence
    // that skipped.
    assert_eq!(cursor.offer(2).await, SeqVerdict::Dedupe);
    assert_eq!(
        cursor.last_admitted().await,
        Some(2),
        "a dedupe does not move the cursor"
    );
    assert_eq!(
        cursor.offer(3).await,
        SeqVerdict::Admit,
        "the successor is still 3"
    );
}

#[tokio::test]
async fn a_sequence_that_skips_is_refused_rather_than_filled_in() {
    let cursor = ClientSeqCursor::new();
    cursor.offer(1).await;

    // A gap is a hole in the worker's durable log that the coordinator cannot
    // distinguish from a lost frame, so it is refused with both values named —
    // the worker log that says what it sent and the coordinator log that says
    // what it was waiting for.
    assert_eq!(
        cursor.offer(5).await,
        SeqVerdict::Gap {
            expected: 2,
            offered: 5
        }
    );
    assert_eq!(
        cursor.last_admitted().await,
        Some(1),
        "a refusal is not progress"
    );
    assert!(
        !SeqVerdict::Gap {
            expected: 2,
            offered: 5
        }
        .admits()
    );
    assert!(
        SeqVerdict::Dedupe.admits(),
        "a dedupe earns an ACK even though it writes nothing"
    );
}

#[tokio::test]
async fn a_worker_that_does_not_start_at_one_is_refused_rather_than_adopted() {
    let cursor = ClientSeqCursor::new();
    // Adopting an arbitrary start would make the gap undetectable, because
    // there would be no gap to detect it against.
    assert_eq!(
        cursor.offer(9).await,
        SeqVerdict::Gap {
            expected: 1,
            offered: 9
        }
    );
}

#[tokio::test]
async fn one_workers_cursor_survives_another_and_a_reconnect() {
    let cursors = ClientSeqCursors::new();
    let mine = cursors.for_worker("a".repeat(64).as_str());
    let theirs = cursors.for_worker("b".repeat(64).as_str());

    mine.offer(1).await;
    mine.offer(2).await;
    theirs.offer(1).await;
    assert_eq!(
        mine.last_admitted().await,
        Some(2),
        "one worker's progress is not another's"
    );
    assert_eq!(theirs.last_admitted().await, Some(1));

    // A reconnect asks the registry again and gets the SAME cursor, because the
    // outbox it is about to replay is the same outbox.
    let reconnected = cursors.for_worker("a".repeat(64).as_str());
    assert_eq!(reconnected.last_admitted().await, Some(2));
    assert_eq!(
        reconnected.offer(2).await,
        SeqVerdict::Dedupe,
        "a replayed frame dedupes"
    );
    assert_eq!(reconnected.offer(3).await, SeqVerdict::Admit);
}
