//! Behaviour tests for the `message` tool between a parent and its subagents:
//! a direct message starts a run for an idle main conversation but only queues
//! for a subagent until its task starts, broadcasts buffer without waking
//! anyone, a peer message ends a `wait`, and a finished subagent refuses messages.

#[path = "support/mod.rs"]
mod support;

use std::sync::atomic::Ordering;
use std::time::Duration;

use roost_agent::{AgentStore, Entry};
use support::{Harness, text_reply, tool_reply, user_texts};

/// Creates a subagent conversation under `parent`, the way `task()` links one.
async fn seed_child(harness: &Harness, parent: &str, title: &str) -> String {
    let child = harness.conversation().await;
    let mut record = harness
        .store
        .conversation(&child)
        .await
        .expect("store reads")
        .expect("the conversation exists");
    let revision = record.revision;
    record.parent_id = Some(parent.to_owned());
    record.title = title.to_owned();
    harness
        .store
        .save_conversation(&record, revision)
        .await
        .expect("the link is saved");
    child
}

fn direct(from_id: &str, text: &str) -> Entry {
    Entry::PeerMessage {
        from_id: from_id.to_owned(),
        from: "main".to_owned(),
        broadcast: false,
        text: text.to_owned(),
    }
}

fn tool_results(messages: &[roost_llm::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            roost_llm::Message::ToolResult { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn direct_message_to_an_idle_subagent_waits_for_its_task() {
    let harness = Harness::new();
    let parent = harness.conversation().await;
    let child = seed_child(&harness, &parent, "scout: find files").await;
    harness
        .runtime
        .deliver_peer_message(&child, direct(&parent, "check the logs"), false)
        .await
        .expect("delivery is accepted");
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        harness.llm.requests_for(&child).is_empty(),
        "a queued message must not start a run for an idle subagent"
    );

    harness.llm.script(&child, text_reply("done"));
    harness.submit(&child, "find files").await;
    harness.settle(&child).await;
    let requests = harness.llm.requests_for(&child);
    let first = requests.first().expect("the subagent runs its task");
    let delivered = user_texts(first).join("\n");
    assert!(delivered.contains("check the logs"), "{delivered}");
}

#[tokio::test]
async fn direct_message_starts_a_run_on_an_idle_main_conversation() {
    let harness = Harness::new();
    let main = harness.conversation().await;
    let child = seed_child(&harness, &main, "scout: find files").await;
    harness.llm.script(&main, text_reply("noted"));
    harness
        .runtime
        .deliver_peer_message(&main, direct(&child, "scout is done"), true)
        .await
        .expect("delivery is accepted");
    harness.settle(&main).await;

    let requests = harness.llm.requests_for(&main);
    let last = requests
        .last()
        .expect("the idle main conversation runs a turn");
    let delivered = user_texts(last).join("\n");
    assert!(delivered.contains("scout is done"), "{delivered}");
}

#[tokio::test]
async fn broadcast_to_an_idle_peer_buffers_without_starting_a_run() {
    let harness = Harness::new();
    let parent = harness.conversation().await;
    let child = seed_child(&harness, &parent, "scout: find files").await;
    harness
        .runtime
        .deliver_peer_message(
            &child,
            Entry::PeerMessage {
                from_id: parent.clone(),
                from: "main".to_owned(),
                broadcast: true,
                text: "status?".to_owned(),
            },
            false,
        )
        .await
        .expect("delivery is accepted");
    harness.settle(&child).await;

    assert!(
        harness.llm.requests_for(&child).is_empty(),
        "a broadcast must not wake an idle peer"
    );
    let entries = harness.store.entries(&child).await.expect("journal reads");
    assert!(
        entries.iter().any(|(_, entry)| matches!(
            entry,
            Entry::PeerMessage { broadcast: true, text, .. } if text == "status?"
        )),
        "the broadcast is journaled for the peer's next turn"
    );
}

#[tokio::test]
async fn subagent_broadcast_reports_delivery_count_to_the_sender() {
    let harness = Harness::new();
    let parent = harness.conversation().await;
    let child = seed_child(&harness, &parent, "scout: find files").await;
    harness.llm.script(
        &child,
        tool_reply(&[(
            "call-1",
            "message",
            r#"{"to":"all","message":"found 3 files"}"#,
        )]),
    );
    harness.llm.script(&child, text_reply("reported"));
    harness.submit(&child, "start the scan").await;
    harness.settle(&child).await;

    let requests = harness.llm.requests_for(&child);
    let last = requests
        .last()
        .expect("the child asks again after the tool");
    let results = tool_results(&last.messages);
    assert!(
        results
            .iter()
            .any(|text| text.contains("Broadcast delivered to 1 of 1 peer(s).")),
        "{results:?}"
    );
    let entries = harness.store.entries(&parent).await.expect("journal reads");
    assert!(
        entries.iter().any(|(_, entry)| matches!(
            entry,
            Entry::PeerMessage { broadcast: true, text, .. } if text == "found 3 files"
        )),
        "the parent's journal holds the broadcast"
    );
}

#[tokio::test]
async fn message_to_an_unknown_recipient_is_an_error_result() {
    let harness = Harness::new();
    let parent = harness.conversation().await;
    harness.llm.script(
        &parent,
        tool_reply(&[("call-1", "message", r#"{"to":"ghost","message":"hi"}"#)]),
    );
    harness.llm.script(&parent, text_reply("ok"));
    harness.submit(&parent, "tell ghost hello").await;
    harness.settle(&parent).await;

    let requests = harness.llm.requests_for(&parent);
    let last = requests
        .last()
        .expect("the parent asks again after the tool");
    let results = tool_results(&last.messages);
    assert!(
        results
            .iter()
            .any(|text| text.contains("No visible peer named `ghost`")),
        "{results:?}"
    );
}

#[tokio::test]
async fn message_to_a_finished_subagent_is_refused() {
    let harness = Harness::new();
    let parent = harness.conversation().await;
    let child = seed_child(&harness, &parent, "scout: find files").await;
    let yielded = Entry::RunIntent {
        active: false,
        through_seq: 0,
        aborted: false,
        yielded: true,
    };
    harness
        .store
        .append_entry(&child, &yielded, &[], None)
        .await
        .expect("the yield is journaled");
    harness.llm.script(
        &parent,
        tool_reply(&[(
            "call-1",
            "message",
            format!(r#"{{"to":"{child}","message":"hi"}}"#).as_str(),
        )]),
    );
    harness.llm.script(&parent, text_reply("ok"));
    harness.submit(&parent, "ping the scout").await;
    harness.settle(&parent).await;

    let requests = harness.llm.requests_for(&parent);
    let last = requests
        .last()
        .expect("the parent asks again after the tool");
    let results = tool_results(&last.messages);
    assert!(
        results
            .iter()
            .any(|text| text.contains("has finished and cannot receive messages")),
        "{results:?}"
    );
    assert!(
        harness.llm.requests_for(&child).is_empty(),
        "a finished subagent takes no run"
    );
}

#[tokio::test]
async fn wait_returns_when_a_peer_message_arrives() {
    let harness = Harness::new();
    let main = harness.conversation().await;
    let child = seed_child(&harness, &main, "scout: find files").await;
    harness.tools.running_jobs.store(1, Ordering::SeqCst);
    harness
        .llm
        .script(&main, tool_reply(&[("call-1", "wait", "{}")]));
    harness.llm.script(&main, text_reply("done waiting"));
    harness.llm.script(&main, text_reply("late"));
    let follow_up_gate = harness.llm.hold_stream(&main, 2);
    harness.submit(&main, "wait for the scout").await;
    // A message that lands before the wait listens is queued and read at the next
    // boundary, so keep delivering until the follow-up stream starts: that start
    // proves the wait ended.
    while tokio::time::timeout(Duration::from_millis(10), follow_up_gate.entered.notified())
        .await
        .is_err()
    {
        harness
            .runtime
            .deliver_peer_message(&main, direct(&child, "scout result ready"), true)
            .await
            .expect("delivery is accepted");
    }
    follow_up_gate.released.notify_one();
    harness.settle(&main).await;

    let requests = harness.llm.requests_for(&main);
    let follow_up = requests
        .get(1)
        .expect("the model is asked again after wait");
    let answered = tool_results(&follow_up.messages)
        .iter()
        .any(|text| text.contains("Wait ended"));
    assert!(answered, "a peer message must end the wait");
}

#[tokio::test]
async fn a_subagent_messages_its_grandchild_it_can_see() {
    let harness = Harness::new();
    let main = harness.conversation().await;
    let relay = seed_child(&harness, &main, "lead: relay the brief").await;
    let worker = seed_child(&harness, &relay, "worker: write the brief").await;
    let args = format!(r#"{{"to":"{worker}","message":"start now"}}"#);
    harness
        .llm
        .script(&relay, tool_reply(&[("call-1", "message", args.as_str())]));
    harness.llm.script(&relay, text_reply("sent"));
    harness.submit(&relay, "relay").await;
    harness.settle(&relay).await;

    let requests = harness.llm.requests_for(&relay);
    let follow_up = requests.last().expect("the relay is asked again");
    let results = tool_results(&follow_up.messages);
    assert!(
        results
            .iter()
            .any(|text| text.contains("queued for worker")),
        "{results:?}"
    );
}
