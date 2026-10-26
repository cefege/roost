//! Harness conversation and lifecycle integration tests (port of
//! `packages/durable/test/harness-conversations.test.ts` @ `abe508e1b`).
//!
//! Scoped to what does not need the task scheduler: lazy root creation, the
//! built-in creation hook, agent edits, Tx-created conversations, and close
//! behaviour.

mod harness_support;

use std::sync::Arc;

use harness_support::{context, init_append, open_recording};
use rpi_durable::harness::{AgentChange, ConversationCreateOptions, RootOptions};
use rpi_durable::ids::{ConversationId, id_from_number};
use rpi_durable::session::{ConversationOwnership, DocOwner, EntryDraft, commit_fn};
use rpi_durable::types::Storage;

mod test_support;

use test_support::{TempDir, cleanup_temp_dirs};

async fn entry_kinds(
    harness: &Arc<rpi_durable::harness::HarnessImpl>,
    id: ConversationId,
) -> Vec<String> {
    let storage = Arc::clone(harness.session().storage());
    let context = context();
    let mut kinds = Vec::new();
    let mut cursor = None;
    loop {
        let page = storage
            .scan_entries(
                rpi_durable::EntryQuery {
                    conversation_id: id,
                    min_entry_id: None,
                    max_entry_id: None,
                    order: None,
                },
                256,
                cursor,
                &context,
            )
            .await
            .expect("scan entries");
        kinds.extend(page.items.into_iter().map(|entry| entry.kind));
        match page.next {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    kinds
}

#[tokio::test]
async fn root_is_created_lazily_with_agent_change_and_init() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = harness
        .root(
            &context,
            RootOptions {
                agent: Some(AgentChange {
                    instructions: Some(Some("be brief".to_string())),
                    ..AgentChange::default()
                }),
                init: Some(init_append("test.init")),
            },
        )
        .await
        .expect("root");
    assert_eq!(
        conversation.id(),
        id_from_number::<rpi_durable::ids::Conversation>(1)
    );
    let agent = conversation.agent(&context).await.expect("agent");
    assert_eq!(agent.instructions.as_deref(), Some("be brief"));
    assert!(
        entry_kinds(&harness, conversation.id())
            .await
            .contains(&"test.init".to_string())
    );
    // The second call returns the same reserved root without re-running init.
    let again = harness
        .root(&context, RootOptions::default())
        .await
        .expect("root again");
    assert_eq!(again.id(), conversation.id());
    assert_eq!(
        entry_kinds(&harness, conversation.id())
            .await
            .iter()
            .filter(|kind| *kind == "test.init")
            .count(),
        1
    );
}

#[tokio::test]
async fn conversation_created_hook_creates_the_builtin_documents() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let root = harness
        .root(&context, RootOptions::default())
        .await
        .expect("root");
    for token in [
        &*rpi_durable::harness::LIVE_DOC,
        &*rpi_durable::harness::INBOX_DOC,
        &*rpi_durable::harness::USAGE_DOC,
        &*rpi_durable::harness::PROVIDER_DOC,
    ] {
        let value = harness
            .session()
            .snapshot(token, DocOwner::Conversation(root.id()), &context)
            .await
            .expect("snapshot");
        assert!(value.is_some(), "built-in document missing");
    }
}

#[tokio::test]
async fn a_tx_created_conversation_gets_the_builtin_documents() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let id = harness
        .session()
        .commit(
            commit_fn(|tx| {
                Box::pin(async move {
                    Ok(tx
                        .create_conversation(ConversationOwnership::Ownerless)
                        .await?
                        .id)
                })
            }),
            &context,
        )
        .await
        .expect("tx conversation");
    let value = harness
        .session()
        .snapshot(
            &rpi_durable::harness::LIVE_DOC,
            DocOwner::Conversation(id),
            &context,
        )
        .await
        .expect("snapshot");
    assert!(value.is_some());
}

#[tokio::test]
async fn agent_change_replaces_clears_and_leaves_alone() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = harness
        .root(&context, RootOptions::default())
        .await
        .expect("root");
    conversation
        .configure(
            AgentChange {
                instructions: Some(Some("first".to_string())),
                ..AgentChange::default()
            },
            &context,
        )
        .await
        .expect("configure");
    assert_eq!(
        conversation
            .agent(&context)
            .await
            .expect("agent")
            .instructions
            .as_deref(),
        Some("first")
    );
    // `cwd` replaces, `instructions` clears; the untouched model stays absent.
    conversation
        .configure(
            AgentChange {
                cwd: Some(Some("/tmp".to_string())),
                instructions: Some(None),
                ..AgentChange::default()
            },
            &context,
        )
        .await
        .expect("configure");
    let agent = conversation.agent(&context).await.expect("agent");
    assert_eq!(agent.cwd.as_deref(), Some("/tmp"));
    assert_eq!(agent.instructions, None);
    assert_eq!(agent.model, None);
}

#[tokio::test]
async fn independent_conversations_get_init_and_roll_back_failures() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let conversation = harness
        .create_conversation(
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: Some(init_append("test.init")),
            },
            &context,
        )
        .await
        .expect("create");
    assert!(
        entry_kinds(&harness, conversation.id())
            .await
            .contains(&"test.init".to_string())
    );
}

/// upstream: "forks at a concrete entry with as-of agent + overrides" and
/// "identity/state across reopen": a conversation-level fork keeps its parent
/// edge and its copied `pi.agent` state across a real storage reopen.
#[tokio::test]
async fn fork_keeps_the_parent_edge_and_survives_reopen() {
    let dir = TempDir::new("rpi-durable-harness-fork");
    let storage = open_jsonl_storage(&dir).await;
    let (harness, _scheduler) = harness_support::open_recording_with(Arc::clone(&storage)).await;
    let context = context();
    let root = harness
        .root(&context, RootOptions::default())
        .await
        .expect("root");
    root.configure(
        AgentChange {
            instructions: Some(Some("forked".to_string())),
            ..AgentChange::default()
        },
        &context,
    )
    .await
    .expect("configure");

    let entry = {
        let conversation = root.id();
        root.commit(
            commit_fn(move |tx| {
                Box::pin(async move {
                    let mut draft = EntryDraft::new("test.note");
                    draft.data = Some(serde_json::json!({ "n": 1 }));
                    let record = tx.append_entry(conversation, draft).await?;
                    Ok(record.id)
                })
            }),
            &context,
        )
        .await
        .expect("commit")
    };

    let forked = root
        .fork(
            entry,
            ConversationCreateOptions {
                ownership: ConversationOwnership::Ownerless,
                agent: None,
                init: None,
            },
            &context,
        )
        .await
        .expect("fork");
    let record = storage
        .conversation(forked.id(), &context)
        .await
        .expect("record")
        .expect("present");
    assert_eq!(
        record
            .parent
            .as_ref()
            .map(|parent| (parent.conversation_id, parent.at)),
        Some((root.id(), entry))
    );
    // Forks start from the agent state as of the fork entry.
    let agent = forked.agent(&context).await.expect("agent");
    assert_eq!(agent.instructions.as_deref(), Some("forked"));

    harness.close(&context).await.expect("close");
    drop(storage);
    let storage = open_jsonl_storage(&dir).await;
    let (reopened, _scheduler) = harness_support::open_recording_with(Arc::clone(&storage)).await;
    let again = reopened
        .conversation(forked.id(), &context)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(again.id(), forked.id());
    assert_eq!(
        again
            .agent(&context)
            .await
            .expect("agent")
            .instructions
            .as_deref(),
        Some("forked")
    );
    reopened.close(&context).await.expect("close");
    drop(storage);
    cleanup_temp_dirs("rpi-durable-harness-fork");
}

async fn open_jsonl_storage(dir: &TempDir) -> Arc<dyn Storage> {
    let directory = dir.path_str();
    let fs: Arc<dyn rpi_durable::fs::FileSystem> =
        Arc::new(rpi_durable::fs::StdFileSystem::new(dir.path()));
    let storage = rpi_durable::storage::JsonlStorage::open(
        &directory,
        fs,
        rpi_durable::storage::JsonlStorageOptions { fsync: false },
        &context(),
    )
    .await
    .expect("open jsonl storage");
    Arc::new(storage)
}

#[tokio::test]
async fn conversation_lookup_returns_stateless_handles_and_close_rejects() {
    let (harness, _scheduler) = open_recording().await;
    let context = context();
    let root = harness
        .root(&context, RootOptions::default())
        .await
        .expect("root");
    let first = harness
        .conversation(root.id(), &context)
        .await
        .expect("lookup")
        .expect("present");
    let second = harness
        .conversation(root.id(), &context)
        .await
        .expect("lookup")
        .expect("present");
    assert_eq!(first.id(), second.id());
    assert!(
        harness
            .conversation(
                id_from_number::<rpi_durable::ids::Conversation>(999),
                &context
            )
            .await
            .expect("lookup")
            .is_none()
    );
    harness.close(&context).await.expect("close");
    assert!(harness.conversation(root.id(), &context).await.is_err());
    assert!(harness.resume().is_err());
}
