//! Snapshot replay must honor withdrawals committed before their event was stored.
//! The real journal and durable event writer model the crash boundary directly.
//! Historical queued/delivered frames must not resurrect the withdrawn input.

use std::sync::Arc;

use roost_agent::traits::{AdmitOutcome, CommandAdmission, IntentChange};
use roost_agent::{AgentStore, ChatSink, Entry};
use roost_protocol::wire::agent_chat::{ChatEvent, QueuedMessage, TranscriptItem};

#[tokio::test]
async fn snapshot_does_not_restore_input_withdrawn_before_its_removal_event() {
    let root = super::scratch("withdraw-snapshot");
    let replica = super::replica(&root, "coord-inbox", Arc::default(), None).await;
    let id = super::create(&replica).await;
    let journal = super::store(&replica);
    journal
        .append_entry(
            &id,
            &Entry::User {
                text: "withdraw this steer".to_owned(),
                loop_repeat: false,
                item_id: Some("queued-request".to_owned()),
            },
            &[],
            None,
        )
        .await
        .unwrap();
    journal
        .append_entry(
            &id,
            &Entry::RunIntent {
                active: true,
                through_seq: 1,
                aborted: false,
                yielded: false,
            },
            &[],
            None,
        )
        .await
        .unwrap();
    replica
        .service
        .sink
        .events(
            &id,
            vec![ChatEvent::Reset {
                transcript: roost_protocol::wire::agent_chat::Transcript {
                    items: vec![TranscriptItem::User {
                        id: "queued-request".to_owned(),
                        text: "withdraw this steer".to_owned(),
                    }],
                    run_state: roost_protocol::wire::agent_chat::AgentRunState::Idle,
                    error: None,
                    model: None,
                    thinking_level: None,
                    mode: None,
                    usage: Default::default(),
                    queued: vec![QueuedMessage {
                        id: "queued-request".to_owned(),
                        text: "withdraw this steer".to_owned(),
                        mode: String::new(),
                    }],
                    schedules: Vec::new(),
                },
            }],
        )
        .await;
    assert!(matches!(
        journal
            .admit_command(
                &id,
                CommandAdmission {
                    request_id: "withdraw-request".to_owned(),
                    kind: "withdraw".to_owned(),
                    entries: vec![Entry::QueuedInputWithdrawn {
                        submission_id: "queued-request".to_owned()
                    }],
                    intent: IntentChange::Withdraw,
                    fence: None,
                    expected_intent_seq: None,
                    completion_through: None,
                }
            )
            .await
            .unwrap(),
        AdmitOutcome::Admitted { .. }
    ));
    let (_, snapshot) = replica.service.sink.snapshot(&id).await.unwrap();
    assert!(snapshot.queued.is_empty());
    assert!(!snapshot.items.iter().any(|item| matches!(item,
        TranscriptItem::User { id, .. } if id == "queued-request")));
    assert_eq!(super::primary_requests(&replica), 0);
    replica.db.pool().close().await;
    let _ = std::fs::remove_dir_all(root);
}
