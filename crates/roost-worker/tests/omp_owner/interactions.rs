//! Pending ACP forms remain explicit, validated and bound to one child request.

use super::{
    DuplexLauncher, OmpOwner, OmpPort, OmpWorkerEvent, UplinkReceiver, frame, prompt, turn, uplink,
};
use roost_proto::{DOmpCancel, DOmpRespond};
use std::sync::Arc;

async fn pending_interaction(
    receiver: &mut UplinkReceiver,
) -> roost_protocol::wire::agent_chat::OmpInteraction {
    loop {
        let batch = frame(receiver).await;
        for encoded in batch.events_json {
            let Ok(event) = serde_json::from_str::<OmpWorkerEvent>(&encoded) else {
                continue;
            };
            if let OmpWorkerEvent::Interaction { interaction } = event {
                return interaction;
            }
        }
    }
}

#[tokio::test]
async fn omp_form_response_stays_pending_until_valid_answer_and_rejects_stale_id() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        elicitation: true,
        ..Default::default()
    };
    let outcome = Arc::clone(&launcher.elicitation_outcome);
    let owner = OmpOwner::new(Arc::new(launcher), uplink);
    owner.prompt(prompt(""));
    let interaction = pending_interaction(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 0);
    owner.respond(DOmpRespond {
        conversation_id: "c1".into(),
        request_id: interaction.id.clone(),
        action: "accept".into(),
        content_json: r#"{"ok":"not-a-boolean"}"#.into(),
        ..Default::default()
    });
    owner.respond(DOmpRespond {
        conversation_id: "c1".into(),
        request_id: interaction.id.clone(),
        action: "accept".into(),
        content_json: r#"{"ok":true}"#.into(),
        ..Default::default()
    });
    let (_, events) = turn(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(events.iter().any(|event| matches!(
        event,
        OmpWorkerEvent::InteractionResolved { id } if id == &interaction.id
    )));

    owner.respond(DOmpRespond {
        conversation_id: "c1".into(),
        request_id: interaction.id,
        action: "accept".into(),
        content_json: r#"{"ok":true}"#.into(),
        ..Default::default()
    });
    let _ = frame(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 1);
}

#[tokio::test]
async fn omp_form_cancel_reaches_the_original_acp_responder() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        elicitation: true,
        ..Default::default()
    };
    let outcome = Arc::clone(&launcher.elicitation_outcome);
    let owner = OmpOwner::new(Arc::new(launcher), uplink);
    owner.prompt(prompt(""));
    let interaction = pending_interaction(&mut receiver).await;
    owner.respond(DOmpRespond {
        conversation_id: "c1".into(),
        request_id: interaction.id,
        action: "cancel".into(),
        ..Default::default()
    });
    let _ = turn(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 3);
}
#[tokio::test]
async fn omp_startup_form_can_be_answered_before_new_session_returns() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        elicitation_on_new_session: true,
        ..Default::default()
    };
    let outcome = Arc::clone(&launcher.elicitation_outcome);
    let owner = OmpOwner::new(Arc::new(launcher), uplink);
    owner.prompt(prompt(""));
    let interaction = pending_interaction(&mut receiver).await;
    owner.respond(DOmpRespond {
        conversation_id: "c1".into(),
        request_id: interaction.id,
        action: "accept".into(),
        content_json: r#"{"ok":true}"#.into(),
        ..Default::default()
    });
    let (_, events) = turn(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert!(
        events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::TurnEnded { error: None, .. }))
    );
}

#[tokio::test]
async fn omp_startup_cancel_after_form_never_submits_initial_prompt() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        elicitation_on_new_session: true,
        ..Default::default()
    };
    let outcome = Arc::clone(&launcher.elicitation_outcome);
    let owner = OmpOwner::new(Arc::new(launcher), uplink);
    owner.prompt(prompt(""));
    let interaction = pending_interaction(&mut receiver).await;
    assert_eq!(outcome.load(std::sync::atomic::Ordering::Relaxed), 0);
    owner.cancel(DOmpCancel {
        conversation_id: "c1".into(),
        ..Default::default()
    });
    let (_, events) = turn(&mut receiver).await;
    assert!(events.iter().any(|event| matches!(
        event, OmpWorkerEvent::InteractionResolved { id } if id == &interaction.id
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        OmpWorkerEvent::TurnEnded { stop_reason, error: None, .. } if stop_reason == "cancelled"
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, OmpWorkerEvent::TurnStarted { .. }))
    );
}

#[tokio::test]
async fn omp_startup_cancel_before_form_aborts_initialization() {
    let (uplink, mut receiver) = uplink::channel();
    let launcher = DuplexLauncher {
        elicitation_on_new_session: true,
        delay_new_session_form: true,
        ..Default::default()
    };
    let started = Arc::clone(&launcher.new_session_started);
    let release_form = Arc::clone(&launcher.release_new_session_form);
    let owner = OmpOwner::new(Arc::new(launcher), uplink);
    owner.prompt(prompt(""));
    tokio::time::timeout(std::time::Duration::from_secs(5), started.notified())
        .await
        .expect("session initialization started");
    owner.cancel(DOmpCancel {
        conversation_id: "c1".into(),
        ..Default::default()
    });
    release_form.notify_one();
    let (_, events) = turn(&mut receiver).await;
    assert!(!events.iter().any(|event| matches!(
        event,
        OmpWorkerEvent::Interaction { .. } | OmpWorkerEvent::TurnStarted { .. }
    )));
}
