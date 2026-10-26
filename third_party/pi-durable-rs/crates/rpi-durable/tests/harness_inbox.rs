//! Harness inbox / boundary placement integration tests (port of
//! `packages/durable/test/harness-inbox.test.ts` @ `abe508e1b`).
//!
//! The boundary matrix is exercised directly on the Session line: queued
//! submissions and inbox items are seeded in one commit, and the boundary is
//! prepared and applied in a later commit (table reads must precede writes).

mod harness_support;

use std::sync::Arc;

use harness_support::{context, modes, open_recording};
use rpi_ai::types::UserContent;
use rpi_durable::harness::{
    BoundaryAt, BoundaryResult, HarnessImpl, INBOX_DOC, InboxItem, InboxState, QueueMode,
    QueueModes, RootOptions, apply_boundary, entry_to_json, prepare_boundary, remove_inbox_item,
    withdraw_queued_inputs,
};
use rpi_durable::ids::{ConversationId, Entry, SubmissionId, id_from_number};
use rpi_durable::session::{DocOwner, EntryDraft, EntryHead, SubmissionCreate, commit_fn};

#[derive(Clone)]
enum Seed {
    Write(EntryDraft),
    Steer(&'static str),
    FollowUp(&'static str),
}

async fn root(harness: &Arc<HarnessImpl>) -> ConversationId {
    harness
        .root(&context(), RootOptions::default())
        .await
        .expect("root")
        .id()
}

/// Create one queued submission per seed and write the inbox items in one commit.
async fn seed(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
    seeds: Vec<Seed>,
) -> Vec<SubmissionId> {
    let change = commit_fn(move |tx| {
        let seeds = seeds.clone();
        Box::pin(async move {
            let mut items = Vec::new();
            let mut ids = Vec::new();
            for item in seeds {
                match item {
                    Seed::Write(entry) => {
                        let record = tx
                            .create_submission(SubmissionCreate::Write {
                                conversation_id,
                                request_id: None,
                            })
                            .await?;
                        items.push(InboxItem::Write {
                            id: record.id(),
                            entry: entry_to_json(&entry),
                        });
                        ids.push(record.id());
                    }
                    Seed::Steer(text) => {
                        let record = tx
                            .create_submission(SubmissionCreate::Input {
                                conversation_id,
                                request_id: None,
                            })
                            .await?;
                        items.push(InboxItem::Steer {
                            id: record.id(),
                            content: UserContent::Text(text.to_string()),
                        });
                        ids.push(record.id());
                    }
                    Seed::FollowUp(text) => {
                        let record = tx
                            .create_submission(SubmissionCreate::Input {
                                conversation_id,
                                request_id: None,
                            })
                            .await?;
                        items.push(InboxItem::FollowUp {
                            id: record.id(),
                            content: UserContent::Text(text.to_string()),
                        });
                        ids.push(record.id());
                    }
                }
            }
            let inbox = tx
                .doc(&INBOX_DOC, DocOwner::Conversation(conversation_id), None)
                .await?;
            let value = serde_json::to_value(InboxState { items })
                .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
            let mut guard = inbox
                .write()
                .map_err(|error| rpi_durable::SessionError::message(error.to_string()))?;
            *guard = value;
            Ok(ids)
        })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("seed")
}

async fn apply(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
    at: BoundaryAt,
    queue_modes: QueueModes,
) -> BoundaryResult {
    let change = commit_fn(move |tx| {
        Box::pin(async move {
            let boundary = prepare_boundary(tx, conversation_id, queue_modes).await?;
            apply_boundary(tx, boundary, at, 5.0).await
        })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("apply boundary")
}

async fn status(harness: &Arc<HarnessImpl>, id: SubmissionId) -> &'static str {
    harness
        .session()
        .storage()
        .submission(id, &context())
        .await
        .expect("submission")
        .expect("present")
        .status()
}

async fn inbox_items(
    harness: &Arc<HarnessImpl>,
    conversation_id: ConversationId,
) -> Vec<InboxItem> {
    let value = harness
        .session()
        .snapshot(
            &INBOX_DOC,
            DocOwner::Conversation(conversation_id),
            &context(),
        )
        .await
        .expect("inbox")
        .expect("present");
    let state: InboxState =
        serde_json::from_value(rpi_durable::JsonValue::Object(value)).expect("inbox state");
    state.items
}

#[tokio::test]
async fn post_tools_places_writes_then_steers_and_holds_follow_ups() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let ids = seed(
        &harness,
        conversation,
        vec![
            Seed::Write(EntryDraft::new("test.note")),
            Seed::Steer("steer"),
            Seed::FollowUp("later"),
        ],
    )
    .await;
    let result = apply(&harness, conversation, BoundaryAt::PostTools, modes()).await;
    assert!(!result.reset);
    assert_eq!(result.users, vec![ids[1]]);
    assert_eq!(status(&harness, ids[0]).await, "done");
    assert_eq!(status(&harness, ids[1]).await, "placed");
    assert_eq!(status(&harness, ids[2]).await, "queued");
}

#[tokio::test]
async fn final_places_one_follow_up_by_default() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let ids = seed(
        &harness,
        conversation,
        vec![
            Seed::Steer("s1"),
            Seed::Steer("s2"),
            Seed::FollowUp("f1"),
            Seed::FollowUp("f2"),
        ],
    )
    .await;
    let result = apply(&harness, conversation, BoundaryAt::Final, modes()).await;
    assert_eq!(result.users, vec![ids[0], ids[2]]);
    assert_eq!(status(&harness, ids[1]).await, "queued");
    assert_eq!(status(&harness, ids[3]).await, "queued");
}

#[tokio::test]
async fn all_mode_places_every_follow_up() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let ids = seed(
        &harness,
        conversation,
        vec![Seed::FollowUp("f1"), Seed::FollowUp("f2")],
    )
    .await;
    let all = QueueModes {
        steering_mode: QueueMode::All,
        follow_up_mode: QueueMode::All,
    };
    let result = apply(&harness, conversation, BoundaryAt::Final, all).await;
    assert_eq!(result.users, ids);
}

#[tokio::test]
async fn a_reset_write_turns_post_tools_into_final() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let mut reset = EntryDraft::new("pi.reset");
    reset.head = Some(EntryHead::This);
    let ids = seed(
        &harness,
        conversation,
        vec![Seed::Write(reset), Seed::FollowUp("new-context")],
    )
    .await;
    let result = apply(&harness, conversation, BoundaryAt::PostTools, modes()).await;
    assert!(result.reset);
    assert_eq!(result.users, vec![ids[1]]);
    assert_eq!(status(&harness, ids[0]).await, "done");
}

#[tokio::test]
async fn a_head_write_before_the_active_range_is_stale() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    // A reset entry sets the active range's head.
    let marker = {
        let change = commit_fn(move |tx| {
            Box::pin(async move {
                let mut entry = EntryDraft::new("pi.reset");
                entry.head = Some(EntryHead::This);
                Ok(tx.append_entry(conversation, entry).await?.id)
            })
        });
        harness
            .session()
            .commit(change, &context())
            .await
            .expect("marker")
    };
    let mut stale = EntryDraft::new("test.note");
    stale.head = Some(EntryHead::At(id_from_number::<Entry>(
        marker.get().saturating_sub(1).max(1),
    )));
    let ids = seed(&harness, conversation, vec![Seed::Write(stale)]).await;
    let result = apply(&harness, conversation, BoundaryAt::Final, modes()).await;
    assert!(result.users.is_empty());
    assert_eq!(status(&harness, ids[0]).await, "unanswered");
}

#[tokio::test]
async fn withdraw_queued_inputs_settles_inputs_and_keeps_writes() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let ids = seed(
        &harness,
        conversation,
        vec![Seed::Steer("s1"), Seed::Write(EntryDraft::new("test.note"))],
    )
    .await;
    let change = commit_fn(move |tx| {
        Box::pin(async move { withdraw_queued_inputs(tx, conversation).await })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("withdraw");
    assert_eq!(status(&harness, ids[0]).await, "unanswered");
    assert_eq!(status(&harness, ids[1]).await, "queued");
    let items = inbox_items(&harness, conversation).await;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].mode(), "write");
}

#[tokio::test]
async fn remove_inbox_item_drops_the_item() {
    let (harness, _scheduler) = open_recording().await;
    let conversation = root(&harness).await;
    let ids = seed(&harness, conversation, vec![Seed::Steer("s1")]).await;
    let target = ids[0];
    let change = commit_fn(move |tx| {
        Box::pin(async move { remove_inbox_item(tx, conversation, target).await })
    });
    harness
        .session()
        .commit(change, &context())
        .await
        .expect("remove");
    assert!(inbox_items(&harness, conversation).await.is_empty());
}
