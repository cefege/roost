//! Native engine behavior when a durable projection read fails after commit.
//! The real engine must keep its settled outcome when a notification read fails.
//! Subsequent model work exercises the same persisted kernel, not a new mock.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{AgentStore, Entry};
use roost_protocol::wire::agent_chat::{AgentRunState, TranscriptItem};
use support::{Harness, PublicationKind, text_reply, tool_reply};

#[tokio::test]
async fn assistant_projection_read_failure_does_not_poison_the_next_request() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("completed once"));
    gate.fail_next_publication_read(PublicationKind::Assistant);

    harness.submit(&id, "first request").await;
    let first = harness.settle(&id).await;
    assert_eq!(first.run_state, AgentRunState::Idle);
    assert_eq!(
        support::texts(&first)
            .iter()
            .filter(|text| *text == "completed once")
            .count(),
        1
    );
    assert_eq!(gate.publication_failures_consumed(), 1);
    assert!(
        harness
            .store
            .entries(&id)
            .await
            .unwrap()
            .iter()
            .any(|(_, entry)| matches!(
                entry,
                Entry::Assistant { blocks, .. } if blocks.iter().any(|block| matches!(block,
                    roost_llm::AssistantBlock::Text { text, .. } if text == "completed once"))
            ))
    );

    harness
        .llm
        .script(&id, text_reply("second request completed"));
    harness.submit(&id, "second request").await;
    let second = harness.settle(&id).await;
    assert_eq!(second.run_state, AgentRunState::Idle);
    assert_eq!(harness.llm.requests_for(&id).len(), 2);
    assert_eq!(
        support::texts(&second)
            .iter()
            .filter(|text| *text == "completed once")
            .count(),
        1
    );
    assert_eq!(
        support::texts(&second)
            .iter()
            .filter(|text| *text == "second request completed")
            .count(),
        1
    );
    assert_eq!(gate.publication_failures_consumed(), 1);
}

#[tokio::test]
async fn tool_result_projection_read_failure_does_not_repeat_tool_effect() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    harness.tools.answer("read", "persisted fixture");
    harness.llm.script(
        &id,
        tool_reply(&[("read-once", "read", r#"{"path":"fixture.txt"}"#)]),
    );
    harness.llm.script(&id, text_reply("read confirmed once"));
    gate.fail_next_publication_read(PublicationKind::ToolResult);

    harness.submit(&id, "read the fixture").await;
    let first = harness.settle(&id).await;
    assert_eq!(first.run_state, AgentRunState::Idle);
    assert_eq!(gate.publication_failures_consumed(), 1);
    assert_eq!(harness.tools.calls_to("read").len(), 1);
    assert!(
        support::texts(&first)
            .iter()
            .any(|text| text == "read confirmed once")
    );
    assert!(harness.store.entries(&id).await.unwrap().iter().any(|(_, entry)| matches!(
        entry,
        Entry::ToolResult { call_id, text, .. } if call_id == "read-once" && text == "persisted fixture"
    )));

    harness.llm.script(&id, text_reply("follow-up completed"));
    harness.submit(&id, "continue after the read").await;
    let second = harness.settle(&id).await;
    assert_eq!(second.run_state, AgentRunState::Idle);
    assert_eq!(harness.llm.requests_for(&id).len(), 3);
    assert_eq!(harness.tools.calls_to("read").len(), 1);
    assert_eq!(
        support::texts(&second)
            .iter()
            .filter(|text| *text == "follow-up completed")
            .count(),
        1
    );
    assert_eq!(gate.publication_failures_consumed(), 1);
    assert!(second.items.iter().any(|item| matches!(item,
        TranscriptItem::Tool { tool_name, output, running: false, .. }
            if tool_name == "read" && output == "persisted fixture"
    )));
}
