//! Regression coverage for SDK callback ordering and transport teardown.
//! Controlled peers exercise the real owner and its uplink event sequence.
//! Process replacement must preserve the conversation actor's run identity.

#[path = "ordering_peer.rs"]
mod ordering_peer;

use self::ordering_peer::{OrderingLauncher, Scenario};
use super::{frame, prompt, text, turn};
use roost_proto::{DOmpClose, WOmpEvents};
use roost_protocol::wire::agent_chat::OmpWorkerEvent;
use roost_worker::{link_ports::OmpPort, omp::OmpOwner, uplink};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::sync::Notify;

async fn notified(signal: &Notify) {
    tokio::time::timeout(Duration::from_secs(5), signal.notified())
        .await
        .expect("fake peer deadline");
}

fn assert_contiguous(frames: &[WOmpEvents]) {
    let run_id = &frames[0].run_id;
    let mut expected_seq = 1;
    for batch in frames {
        assert_eq!(
            &batch.run_id, run_id,
            "process replacement changed the actor run"
        );
        assert_eq!(batch.first_seq, expected_seq);
        expected_seq += batch.events_json.len() as u64;
    }
}

#[tokio::test]
async fn omp_owner_clean_eof_relaunches_without_resetting_event_sequence() {
    let launcher = Arc::new(OrderingLauncher::new(Scenario::Complete));
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    owner.prompt(prompt(""));
    let (mut frames, events) = turn(&mut receiver).await;
    assert!(matches!(events.last(), Some(OmpWorkerEvent::TurnEnded {
        request_id, outstanding: 0, error: None, ..
    }) if request_id == "req1"));

    launcher.disconnect.notify_one();
    let mut exited = false;
    while !exited {
        let batch = frame(&mut receiver).await;
        for json in &batch.events_json {
            if let OmpWorkerEvent::ProcessExited { error } = serde_json::from_str(json).unwrap() {
                assert!(
                    error.is_none(),
                    "clean peer EOF became a failure: {error:?}"
                );
                exited = true;
            }
        }
        frames.push(batch);
    }
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);

    let mut second = prompt("");
    second.request_id = "req2".into();
    owner.prompt(second);
    let (second_frames, events) = turn(&mut receiver).await;
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 2);
    assert!(events.iter().any(|event| matches!(event,
        OmpWorkerEvent::SessionReady { acp_session_id, .. } if acp_session_id == "s1"
    )));
    assert!(matches!(events.last(), Some(OmpWorkerEvent::TurnEnded {
        request_id, outstanding: 0, error: None, ..
    }) if request_id == "req2"));
    frames.extend(second_frames);
    assert_contiguous(&frames);
}

async fn close_stalled_peer(scenario: Scenario) {
    let launcher = Arc::new(OrderingLauncher::new(scenario));
    let (uplink, _receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    owner.prompt(prompt(""));
    notified(&launcher.started).await;
    owner.close(DOmpClose {
        conversation_id: "c1".into(),
        ..Default::default()
    });
    notified(&launcher.closed).await;
    assert_eq!(launcher.launches.load(Ordering::SeqCst), 1);
    // Keep the owner alive until the fake has observed its transport closing.
    drop(owner);
}

#[tokio::test]
async fn omp_owner_close_drops_transport_during_stalled_initialize() {
    close_stalled_peer(Scenario::StallInitialize).await;
}

#[tokio::test]
async fn omp_owner_close_drops_transport_during_stalled_session_new() {
    close_stalled_peer(Scenario::StallSession).await;
}

#[tokio::test]
async fn omp_owner_prompt_responses_stay_between_adjacent_notification_chunks() {
    let launcher = Arc::new(OrderingLauncher::new(Scenario::OrderedPrompts));
    let (uplink, mut receiver) = uplink::channel();
    let owner = OmpOwner::new(Arc::clone(&launcher), uplink);
    owner.prompt(prompt(""));
    let mut second = prompt("");
    second.request_id = "req2".into();
    owner.prompt(second);
    notified(&launcher.queued).await;
    launcher.release.notify_one();

    let mut frames = Vec::new();
    let mut events = Vec::new();
    while !events.iter().any(|event| {
        matches!(event,
            OmpWorkerEvent::TurnEnded { request_id, outstanding: 0, .. } if request_id == "req2"
        )
    }) {
        let batch = frame(&mut receiver).await;
        events.extend(
            batch
                .events_json
                .iter()
                .map(|json| serde_json::from_str::<OmpWorkerEvent>(json).unwrap()),
        );
        frames.push(batch);
    }
    assert_contiguous(&frames);
    assert_eq!(text(&events), vec!["A", "B"]);
    let chunk_position = |expected: &str| {
        events
            .iter()
            .position(|event| {
                matches!(event,
                    OmpWorkerEvent::Update { update } if update["content"]["text"] == expected
                )
            })
            .unwrap()
    };
    let first_end = events
        .iter()
        .position(|event| {
            matches!(event,
                OmpWorkerEvent::TurnEnded { request_id, outstanding: 1, error: None, .. }
                    if request_id == "req1"
            )
        })
        .expect("first response must finish while the second prompt is outstanding");
    assert!(chunk_position("A") < first_end);
    assert!(first_end < chunk_position("B"));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, OmpWorkerEvent::TurnStarted { .. }))
            .count(),
        2
    );
    assert!(matches!(events.last(), Some(OmpWorkerEvent::TurnEnded {
        request_id, outstanding: 0, error: None, ..
    }) if request_id == "req2"));
}

#[tokio::test]
async fn omp_owner_rejects_missing_or_incompatible_semantic_ui_before_session_ready() {
    for scenario in [
        Scenario::MissingCommandUi,
        Scenario::UnsupportedCommandUiVersion,
        Scenario::WrongCommandUiProtocol,
    ] {
        let launcher = Arc::new(OrderingLauncher::new(scenario));
        let (uplink, mut receiver) = uplink::channel();
        let owner = OmpOwner::new(launcher, uplink);
        owner.prompt(prompt(""));
        let (_, events) = turn(&mut receiver).await;
        assert!(
            events
                .iter()
                .any(|event| matches!(event, OmpWorkerEvent::TurnEnded {
                stop_reason, error: Some(_), outstanding: 0, ..
            } if stop_reason == "error")),
            "{scenario:?} must refuse the turn: {events:?}"
        );
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, OmpWorkerEvent::SessionReady { .. })),
            "{scenario:?} must not publish a usable conversation"
        );
    }
}
