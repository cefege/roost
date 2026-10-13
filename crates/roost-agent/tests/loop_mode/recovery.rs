//! Recovery of durable loop configuration and its count/time invariants.

use roost_agent::{AgentStore, Entry};

use super::support::{Harness, text_reply, user_texts};
use super::{LIMIT_REACHED, loop_status, notice_bodies, parked};

#[path = "recovery/completion.rs"]
mod completion;

#[tokio::test]
async fn counted_loop_restores_its_remaining_budget_after_pause() {
    let (harness, gate) = Harness::gated();
    let id = harness.conversation().await;
    gate.hold_user_append(2);
    harness.llm.script(&id, text_reply("first"));
    harness.llm.script(&id, text_reply("resumed"));
    harness.llm.script(&id, text_reply("last run"));
    harness.submit(&id, "/loop 3 bump").await;
    parked(gate.user_append_held()).await;
    assert!(harness.runtime.abort(&id).await.unwrap());
    gate.release_user_append();
    let paused = harness.settle(&id).await;
    assert!(loop_status(&paused.items).is_some_and(|status| status.contains("paused")));

    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let restored = restarted.transcript(&id).await.unwrap();
    assert!(loop_status(&restored.items).is_some_and(|status| status.contains("paused")));
    assert_eq!(harness.llm.requests_for(&id).len(), 1);

    restarted.submit(&id, "again".to_owned()).await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(
        requests.len(),
        3,
        "the reboot must not reset the two runs left"
    );
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| body == LIMIT_REACHED)
    );
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn waiting_loop_is_restored_without_starting_a_run() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 4").await;

    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = restarted.transcript(&id).await.unwrap();

    assert!(loop_status(&transcript.items).is_some_and(|status| {
        status.contains("waiting for a prompt") && status.contains("4 runs left")
    }));
    assert!(harness.llm.requests_for(&id).is_empty());
}

#[tokio::test]
async fn elapsed_duration_is_not_reset_after_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 1s").await;
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = restarted.transcript(&id).await.unwrap();

    assert!(loop_status(&transcript.items).is_none());
    assert!(harness.llm.requests_for(&id).is_empty());
}

#[tokio::test]
async fn off_loop_stays_stopped_after_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 4").await;
    harness.submit(&id, "/loop off").await;

    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let restored = restarted.transcript(&id).await.unwrap();
    assert!(loop_status(&restored.items).is_none());

    harness.llm.script(&id, text_reply("one ordinary run"));
    restarted.submit(&id, "continue".to_owned()).await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;
    assert_eq!(harness.llm.requests_for(&id).len(), 1);
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn committed_repeat_prompt_is_reconciled_once_by_journal_sequence() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 3").await;
    let entries = harness.store.entries(&id).await.unwrap();
    let after_seq = entries.last().map_or(0, |(seq, _)| *seq);
    let snapshot = serde_json::json!({
        "body": "bump",
        "paused": false,
        "budget": {"kind": "runs", "remaining": 2},
        "condition": null,
        "pending_repeat": {"after_seq": after_seq, "body": "bump"},
        "condition_pending": false,
        "condition_ready": false
    })
    .to_string();
    harness
        .store
        .append_entry(
            &id,
            &Entry::LoopState {
                state_json: Some(snapshot),
            },
            &[],
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "bump".to_owned(),
                loop_repeat: true,
            },
            &[],
        )
        .await
        .unwrap();

    harness
        .llm
        .script(&id, text_reply("resumed committed repeat"));
    harness.llm.script(&id, text_reply("last permitted repeat"));
    let restarted = harness.restart_runtime();
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 1);
    assert_eq!(restarted.recover_interrupted_runs().await.unwrap(), 0);
    let transcript = harness.settle_runtime(&restarted, &id).await;
    let requests = harness.llm.requests_for(&id);
    assert_eq!(requests.len(), 2);
    assert_eq!(user_texts(&requests[0]), vec!["bump"]);
    assert_eq!(user_texts(&requests[1]), vec!["bump", "bump"]);
    assert!(loop_status(&transcript.items).is_none());
}

#[tokio::test]
async fn prompt_capture_is_spent_only_when_its_user_entry_committed() {
    for (accepted, remaining) in [(true, 2), (false, 3)] {
        let harness = Harness::new();
        let id = harness.conversation().await;
        harness.submit(&id, "/loop 3").await;
        let after_seq = harness
            .store
            .entries(&id)
            .await
            .unwrap()
            .last()
            .map_or(0, |(seq, _)| *seq);
        let snapshot = serde_json::json!({
            "body": null,
            "paused": false,
            "budget": {"kind": "runs", "remaining": 3},
            "condition": null,
            "pending_repeat": null,
            "pending_capture": {"after_seq": after_seq, "body": "accepted prompt"},
            "condition_pending": false,
            "condition_ready": false
        })
        .to_string();
        harness
            .store
            .append_entry(
                &id,
                &Entry::LoopState {
                    state_json: Some(snapshot),
                },
                &[],
            )
            .await
            .unwrap();
        if accepted {
            harness
                .store
                .append_entry(
                    &id,
                    &Entry::User {
                        text: "accepted prompt".to_owned(),
                        loop_repeat: false,
                    },
                    &[],
                )
                .await
                .unwrap();
        }

        let restarted = harness.restart_runtime();
        restarted.recover_interrupted_runs().await.unwrap();
        let transcript = restarted.transcript(&id).await.unwrap();
        assert!(
            loop_status(&transcript.items)
                .is_some_and(|status| status.contains(&format!("{remaining} runs left")))
        );
        let state_json = harness
            .store
            .entries(&id)
            .await
            .unwrap()
            .into_iter()
            .rev()
            .find_map(|(_, entry)| match entry {
                Entry::LoopState {
                    state_json: Some(json),
                } => Some(json),
                _ => None,
            })
            .unwrap();
        let state: serde_json::Value = serde_json::from_str(&state_json).unwrap();
        assert_eq!(
            state["body"],
            if accepted {
                serde_json::json!("accepted prompt")
            } else {
                serde_json::Value::Null
            }
        );
        assert_eq!(state["pending_capture"], serde_json::Value::Null);
    }
}

#[tokio::test]
async fn an_uncommitted_repeat_marker_survives_recovery() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 3").await;
    let after_seq = harness
        .store
        .entries(&id)
        .await
        .unwrap()
        .last()
        .map_or(0, |(seq, _)| *seq);
    let snapshot = serde_json::json!({
        "body": "bump",
        "paused": false,
        "budget": {"kind": "runs", "remaining": 2},
        "condition": null,
        "pending_repeat": {"after_seq": after_seq, "body": "bump"},
        "pending_capture": null,
        "condition_pending": false,
        "condition_ready": false
    })
    .to_string();
    harness
        .store
        .append_entry(
            &id,
            &Entry::LoopState {
                state_json: Some(snapshot),
            },
            &[],
        )
        .await
        .unwrap();

    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let state_json = harness
        .store
        .entries(&id)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find_map(|(_, entry)| match entry {
            Entry::LoopState {
                state_json: Some(json),
            } => Some(json),
            _ => None,
        })
        .unwrap();
    let state: serde_json::Value = serde_json::from_str(&state_json).unwrap();
    assert_eq!(state["pending_repeat"]["body"], "bump");
}

#[tokio::test]
async fn ambiguous_condition_check_is_not_replayed_after_restart() {
    let harness = Harness::new();
    let id = harness.conversation().await;
    harness.submit(&id, "/loop 3 --until 'bun test'").await;
    let snapshot = serde_json::json!({
        "body": "bump",
        "paused": false,
        "budget": {"kind": "runs", "remaining": 2},
        "condition": {"command": "bun test", "until": true},
        "pending_repeat": null,
        "condition_pending": true,
        "condition_ready": false
    })
    .to_string();
    harness
        .store
        .append_entry(
            &id,
            &Entry::LoopState {
                state_json: Some(snapshot),
            },
            &[],
        )
        .await
        .unwrap();
    harness
        .store
        .append_entry(
            &id,
            &Entry::User {
                text: "bump".to_owned(),
                loop_repeat: false,
            },
            &[],
        )
        .await
        .unwrap();

    harness.llm.script(&id, text_reply("continue safely"));
    let restarted = harness.restart_runtime();
    restarted.recover_interrupted_runs().await.unwrap();
    let transcript = harness.settle_runtime(&restarted, &id).await;

    assert!(harness.tools.calls_to("bash").is_empty());
    assert!(
        notice_bodies(&transcript.items)
            .iter()
            .any(|body| { body.contains("condition result was interrupted") })
    );
}
