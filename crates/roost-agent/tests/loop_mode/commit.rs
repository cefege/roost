//! Repeat budget accounting when a store commit wins the race with Stop.
//! The store parks after writing but before acknowledging the entry, so the
//! resumed loop must count that interrupted repeat without issuing its model call.

use super::support::{Harness, text_reply, user_texts};
use super::{LIMIT_REACHED, loop_status, notice_bodies, parked};

#[tokio::test]
async fn stop_after_repeat_commit_spends_that_started_run() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_committed_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.llm.script(&id, text_reply("resumed"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    assert!(harness.runtime.abort(&id));
    gate.release_user_append();
    let paused = harness.settle(&id).await;
    assert!(loop_status(&paused.items).is_some_and(|status| status.contains("paused")));
    assert_eq!(harness.llm.requests_for(&id).len(), 1);

    harness.submit(&id, "again").await;
    let transcript = harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        2,
        "the committed repeat already spent the second run"
    );
    assert_eq!(user_texts(&requests[1]), vec!["bump", "bump", "again"]);
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn reconfiguring_after_commit_does_not_spend_the_new_loops_budget() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_committed_user_append(2);
    for reply in ["first", "committed", "new first", "new second"] {
        harness.llm.script(&id, text_reply(reply));
    }
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    harness.submit(&id, "/loop 2").await;
    gate.release_user_append();
    let transcript = harness.settle(&id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 4);
    assert_eq!(
        user_texts(&requests[3]),
        vec!["bump", "bump", "bump", "bump"]
    );
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}
