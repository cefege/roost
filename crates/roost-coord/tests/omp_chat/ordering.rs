//! Transaction recovery and transcript ordering at the OMP worker boundary.
//! Uses real coordinator RPCs, SQLite fault injection and worker frames.
//! Writer barriers make negative delivery assertions deterministic.

use super::*;
use roost_coord::db::DbBackend;

async fn install_failure(fixture: &AgentFixture) {
    sqlx::raw_sql(
        "CREATE TRIGGER omp_chat_insert_failure BEFORE INSERT ON agent_chat_events \
         BEGIN SELECT RAISE(FAIL, 'injected failure'); END",
    )
    .execute(fixture.core.services.db.pool())
    .await
    .expect("install failure trigger");
}

async fn drop_failure(fixture: &AgentFixture) {
    sqlx::raw_sql("DROP TRIGGER omp_chat_insert_failure")
        .execute(fixture.core.services.db.pool())
        .await
        .expect("drop failure trigger");
}

async fn barrier(fixture: &AgentFixture, id: &str) {
    fixture
        .core
        .services
        .omp
        .writers
        .local(id, vec![], vec![])
        .await
        .expect("empty writer barrier");
}

fn chunk(text: &str) -> OmpWorkerEvent {
    OmpWorkerEvent::Update {
        update: serde_json::json!({
            "sessionUpdate": "agent_message_chunk",
            "content": {"type": "text", "text": text},
        }),
    }
}

fn feed(fixture: &AgentFixture, id: &str, first_seq: u64, events: &[OmpWorkerEvent]) {
    assert!(
        fixture.core.services.omp.receive(
            WORKER_A,
            CoordWorkerUpstream::OmpEvents(proto::WOmpEvents {
                conversation_id: id.into(),
                run_id: "r1".into(),
                first_seq,
                events_json: events
                    .iter()
                    .map(|event| serde_json::to_string(event).unwrap())
                    .collect(),
                ..Default::default()
            }),
        )
    );
}

async fn expect_ack(
    downstream: &mut mpsc::UnboundedReceiver<CoordWorkerDownstream>,
    id: &str,
    seq: u64,
) {
    assert!(matches!(
        receive(downstream).await,
        CoordWorkerDownstream::OmpEventsAck(ack)
            if ack.conversation_id == id && ack.run_id == "r1" && ack.seq == seq
    ));
}

#[tokio::test]
async fn omp_chat_failed_submit_can_retry_same_request() {
    let fixture = AgentFixture::new("omp-chat-submit-rollback").await;
    if fixture.core.services.db.backend() != DbBackend::Sqlite {
        return;
    }
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    install_failure(&fixture).await;
    let result = handle_agent_chat_submit(
        &fixture.core,
        &fixture.caller,
        proto::AgentChatSubmitRequest {
            conversation_id: id.clone(),
            request_id: "req".into(),
            text: "hello".into(),
            when_busy: "follow_up".into(),
            ..Default::default()
        },
    )
    .await;
    assert!(result.is_err());
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert!(transcript.items.is_empty());
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert!(downstream.try_recv().is_err());

    drop_failure(&fixture).await;
    submit(&fixture, &id, "req").await;
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpPrompt(prompt) if prompt.request_id == "req"
    ));
    assert!(downstream.try_recv().is_err());
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert_eq!(transcript.run_state, AgentRunState::Running);
    assert_eq!(transcript.items.len(), 1);
    assert!(matches!(
        &transcript.items[0],
        TranscriptItem::User { id, text, .. } if id == "u-req" && text == "hello"
    ));
}

#[tokio::test]
async fn omp_chat_failed_worker_batch_fences_later_ack_until_replay() {
    let fixture = AgentFixture::new("omp-chat-output-rollback").await;
    if fixture.core.services.db.backend() != DbBackend::Sqlite {
        return;
    }
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    submit(&fixture, &id, "req").await;
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpPrompt(_)
    ));

    install_failure(&fixture).await;
    feed(&fixture, &id, 1, &[chunk("A")]);
    barrier(&fixture, &id).await;
    assert!(downstream.try_recv().is_err());
    drop_failure(&fixture).await;
    feed(&fixture, &id, 2, &[chunk("B")]);
    barrier(&fixture, &id).await;
    assert!(downstream.try_recv().is_err());
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert!(
        !transcript
            .items
            .iter()
            .any(|item| matches!(item, TranscriptItem::Assistant { .. }))
    );

    feed(&fixture, &id, 1, &[chunk("A")]);
    expect_ack(&mut downstream, &id, 1).await;
    feed(&fixture, &id, 2, &[chunk("B")]);
    expect_ack(&mut downstream, &id, 2).await;
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert_eq!(transcript.items.len(), 2);
    assert!(matches!(
        &transcript.items[1],
        TranscriptItem::Assistant { blocks, .. }
            if blocks == &vec![TranscriptBlock::Text { text: "AB".into() }]
    ));
    assert!(downstream.try_recv().is_err());
}

#[tokio::test]
async fn omp_chat_follow_up_closes_previous_streaming_assistant() {
    let fixture = AgentFixture::new("omp-chat-follow-up-order").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    submit(&fixture, &id, "req-1").await;
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpPrompt(prompt) if prompt.request_id == "req-1"
    ));
    feed(&fixture, &id, 1, &[chunk("A")]);
    expect_ack(&mut downstream, &id, 1).await;
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert!(matches!(
        &transcript.items[1],
        TranscriptItem::Assistant {
            streaming: true,
            ..
        }
    ));

    submit(&fixture, &id, "req-2").await;
    assert!(matches!(
        receive(&mut downstream).await,
        CoordWorkerDownstream::OmpPrompt(prompt) if prompt.request_id == "req-2"
    ));
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert!(matches!(
        &transcript.items[1],
        TranscriptItem::Assistant {
            streaming: false,
            ..
        }
    ));
    feed(
        &fixture,
        &id,
        2,
        &[
            chunk("B"),
            OmpWorkerEvent::TurnEnded {
                request_id: "req-2".into(),
                stop_reason: "end_turn".into(),
                error: None,
                outstanding: 0,
            },
        ],
    );
    expect_ack(&mut downstream, &id, 3).await;
    let (_, transcript) = snapshot(&fixture, &id).await;
    assert_eq!(transcript.run_state, AgentRunState::Idle);
    assert_eq!(transcript.items.len(), 4);
    for (offset, request_id, text) in [(0, "u-req-1", "A"), (2, "u-req-2", "B")] {
        assert!(matches!(
            &transcript.items[offset],
            TranscriptItem::User { id, .. } if id == request_id
        ));
        assert!(matches!(
            &transcript.items[offset + 1],
            TranscriptItem::Assistant { blocks, streaming: false, .. }
                if blocks == &vec![TranscriptBlock::Text { text: text.into() }]
        ));
    }
}

#[tokio::test]
async fn omp_chat_concurrent_submit_matches_worker_and_transcript_order() {
    let fixture = AgentFixture::new("omp-chat-concurrent-submit").await;
    let (_handle, mut downstream) = register_worker(&fixture, "worker-epoch");
    let id = create(&fixture).await;
    tokio::join!(
        submit(&fixture, &id, "req-1"),
        submit(&fixture, &id, "req-2"),
    );
    let mut delivered_ids = Vec::new();
    for _ in 0..2 {
        match receive(&mut downstream).await {
            CoordWorkerDownstream::OmpPrompt(prompt) => {
                delivered_ids.push(format!("u-{}", prompt.request_id));
            }
            frame => panic!("expected prompt, got {frame:?}"),
        }
    }
    let (_, transcript) = snapshot(&fixture, &id).await;
    let user_ids: Vec<_> = transcript
        .items
        .iter()
        .filter_map(|item| match item {
            TranscriptItem::User { id, .. } => Some(id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(user_ids.len(), 2);
    assert_ne!(user_ids[0], user_ids[1]);
    assert_eq!(delivered_ids, user_ids);
    assert!(downstream.try_recv().is_err());
}
