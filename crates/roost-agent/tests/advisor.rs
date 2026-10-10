//! Advisor review, delivery, quarantine, and emission-guard behavior against
//! the scripted model, memory store, and worker seams.

#[path = "support/mod.rs"]
mod support;

use roost_agent::{AgentSettings, AgentStore, Entry};
use roost_llm::Message;
use roost_protocol::wire::agent_chat::TranscriptItem;
use support::{Harness, Script, text_reply, tool_reply};

async fn enable_advisor(harness: &Harness) {
    let settings = AgentSettings {
        advisor_enabled: true,
        ..AgentSettings::default()
    };
    harness.runtime.set_settings(settings).await.unwrap();
}

async fn wait_for_card(
    harness: &Harness,
    id: &str,
) -> roost_protocol::wire::agent_chat::Transcript {
    for _ in 0..100 {
        let transcript = harness.runtime.transcript(id).await.unwrap();
        if transcript
            .items
            .iter()
            .any(|item| matches!(item, TranscriptItem::Advisory { .. }))
        {
            return transcript;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("advisor did not deliver a card for {id}");
}

fn advise_calls(notes: &[(&str, &str)]) -> Script {
    let owned = notes
        .iter()
        .enumerate()
        .map(|(index, (note, severity))| {
            (
                format!("adv-{index}"),
                serde_json::json!({"note": note, "severity": severity}).to_string(),
            )
        })
        .collect::<Vec<_>>();
    let calls = owned
        .iter()
        .map(|(call_id, arguments)| (call_id.as_str(), "advise", arguments.as_str()))
        .collect::<Vec<_>>();
    tool_reply(&calls)
}

fn scripted_advise(note: &str, severity: &str) -> Script {
    let arguments = serde_json::json!({"note": note, "severity": severity}).to_string();
    tool_reply(&[("adv-1", "advise", &arguments)])
}

async fn script_advisor(harness: &Harness, id: &str, scripts: Vec<Script>) {
    for script in scripts {
        harness.llm.script(&format!("{id}-advisor"), script);
    }
}

#[tokio::test]
async fn idle_blocker_starts_one_primary_run_with_advisory_context() {
    let harness = Harness::new();
    enable_advisor(&harness).await;
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("The task is complete."));
    harness
        .llm
        .script(&id, text_reply("I will address the blocker."));
    script_advisor(
        &harness,
        &id,
        vec![
            scripted_advise("The tests contradict the claim.", "blocker"),
            text_reply("Review complete."),
        ],
    )
    .await;

    harness.submit(&id, "Review the change").await;
    for _ in 0..200 {
        if harness.llm.requests_for(&id).len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    harness.settle(&id).await;

    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        2,
        "one blocker review starts exactly one additional run"
    );
    let visible = requests[1]
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User { content } => Some(
                content
                    .iter()
                    .filter_map(|part| match part {
                        roost_llm::UserContent::Text { text } => Some(text.as_str()),
                        roost_llm::UserContent::Image { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        visible.contains("<advisory severity=\"blocker\""),
        "{visible}"
    );
}

#[tokio::test]
async fn concern_after_terminal_answer_stays_an_undelivered_card() {
    let harness = Harness::new();
    enable_advisor(&harness).await;
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("The task is complete."));
    script_advisor(
        &harness,
        &id,
        vec![
            scripted_advise("Consider adding a regression test.", "concern"),
            text_reply("Review complete."),
        ],
    )
    .await;

    harness.submit(&id, "Review the change").await;
    harness.settle(&id).await;
    let transcript = wait_for_card(&harness, &id).await;
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Advisory { severity, delivered: false, .. } if severity == "concern")));
}

#[tokio::test]
async fn concern_in_plan_mode_stays_an_undelivered_card() {
    let harness = Harness::new();
    enable_advisor(&harness).await;
    let id = harness.conversation().await;
    harness.submit(&id, "/plan").await;
    harness.llm.script(&id, text_reply("The plan is ready."));
    script_advisor(
        &harness,
        &id,
        vec![
            scripted_advise("This plan misses a test.", "concern"),
            text_reply("Review complete."),
        ],
    )
    .await;

    harness.submit(&id, "Inspect the plan").await;
    harness.settle(&id).await;
    let transcript = wait_for_card(&harness, &id).await;
    assert!(transcript.items.iter().any(|item| matches!(item,
        TranscriptItem::Advisory { severity, delivered: false, .. } if severity == "concern")));
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
}

#[tokio::test]
async fn duplicate_and_fifth_non_blocker_notes_return_guard_statuses() {
    let harness = Harness::new();
    enable_advisor(&harness).await;
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("Done."));
    let notes = [
        ("Check the edge case", "nit"),
        ("CHECK the edge case!", "nit"),
        ("Second distinct suggestion", "nit"),
        ("Third distinct suggestion", "nit"),
        ("Fourth distinct suggestion", "nit"),
        ("Fifth distinct suggestion", "nit"),
    ];
    script_advisor(
        &harness,
        &id,
        vec![advise_calls(&notes), text_reply("Review complete.")],
    )
    .await;

    harness.submit(&id, "Review these changes").await;
    harness.settle(&id).await;
    for _ in 0..100 {
        if harness.llm.requests_for(&format!("{id}-advisor")).len() >= 2 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let requests = harness.llm.requests_for(&format!("{id}-advisor"));
    assert!(requests.len() >= 2);
    let statuses = requests[1]
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::ToolResult { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "duplicate")
            .count(),
        1
    );
    assert_eq!(
        statuses
            .iter()
            .filter(|status| **status == "budget")
            .count(),
        1
    );
}

#[tokio::test]
async fn destructive_advice_absent_from_the_delta_is_quarantined() {
    let harness = Harness::new();
    enable_advisor(&harness).await;
    let id = harness.conversation().await;
    harness.llm.script(&id, text_reply("The task is complete."));
    script_advisor(
        &harness,
        &id,
        vec![
            scripted_advise("Run rm -rf / to clean this up.", "blocker"),
            text_reply("Review complete."),
        ],
    )
    .await;

    harness.submit(&id, "Review the change").await;
    harness.settle(&id).await;
    for _ in 0..100 {
        let entries = harness.store.entries(&id).await.unwrap();
        if entries.iter().any(|(_, entry)| matches!(entry, Entry::Notice { title, .. } if title == "Advisor response quarantined")) { break; }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let entries = harness.store.entries(&id).await.unwrap();
    assert!(entries.iter().any(|(_, entry)| matches!(entry, Entry::Notice { title, .. } if title == "Advisor response quarantined")), "{entries:#?}");
    assert!(
        !entries
            .iter()
            .any(|(_, entry)| matches!(entry, Entry::Advisory { .. })),
        "{entries:#?}"
    );
}
