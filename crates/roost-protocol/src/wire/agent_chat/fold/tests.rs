//! Exercises transcript replacement, scheduling, and OMP interaction identity.
//! Compiled by the parent fold module as isolated unit cases.
//! Uses the shared fold and wire types consumed by coordinator and browser.

use crate::wire::agent_chat::AgentRunState;

use super::*;
use crate::wire::agent_chat::ScheduledMessage;

fn transcript() -> Transcript {
    Transcript {
        items: Vec::new(),
        run_state: AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        mode: None,
        usage: Default::default(),
        queued: Vec::new(),
        omp_commands: Vec::new(),
        omp_interactions: Vec::new(),
        omp_ui: None,
        omp_ui_transport_id: None,
        omp_ui_error: None,
        schedules: Vec::new(),
        omp_session: None,
    }
}

#[test]
fn native_resume_replaces_history_without_discarding_live_ui_or_configuration() {
    let mut transcript = transcript();
    transcript.mode = Some("plan".into());
    transcript.omp_session = Some(crate::wire::agent_chat::OmpSessionState::Ready);
    transcript.omp_ui = Some(Default::default());
    transcript.omp_ui_transport_id = Some("live-transport".into());
    transcript.omp_ui_error = Some("existing UI diagnostic".into());
    transcript
        .schedules
        .push(schedule("retained-roost-schedule", 100));
    transcript.items.push(TranscriptItem::User {
        id: "previous-session".into(),
        text: "old private context".into(),
        attachment_names: vec![],
    });
    transcript
        .queued
        .push(crate::wire::agent_chat::QueuedMessage {
            id: "old-queued".into(),
            text: "old queued context".into(),
            attachment_names: vec![],
            mode: String::new(),
        });
    fold_chat_event(&mut transcript, &ChatEvent::OmpHistoryReset);
    fold_chat_event(
        &mut transcript,
        &ChatEvent::Item {
            item: TranscriptItem::User {
                id: "resumed-session".into(),
                text: "resumed context".into(),
                attachment_names: vec![],
            },
        },
    );
    assert_eq!(
        transcript.items,
        vec![TranscriptItem::User {
            id: "resumed-session".into(),
            text: "resumed context".into(),
            attachment_names: vec![],
        }]
    );
    assert!(transcript.queued.is_empty());
    assert_eq!(transcript.mode.as_deref(), Some("plan"));
    assert_eq!(
        transcript.omp_session,
        Some(crate::wire::agent_chat::OmpSessionState::Ready)
    );
    assert!(transcript.omp_ui.is_some());
    assert_eq!(
        transcript.omp_ui_transport_id.as_deref(),
        Some("live-transport")
    );
    assert_eq!(
        transcript.omp_ui_error.as_deref(),
        Some("existing UI diagnostic")
    );
    assert_eq!(
        transcript.schedules,
        vec![schedule("retained-roost-schedule", 100)]
    );
}

fn schedule(id: &str, due_ms: i64) -> ScheduledMessage {
    ScheduledMessage {
        id: id.to_owned(),
        text: "ping".to_owned(),
        due_ms,
        every_ms: None,
        fired: 0,
    }
}

#[test]
fn agent_events_preserve_omp_session_unless_explicitly_set() {
    let mut transcript = transcript();
    let state = crate::wire::agent_chat::OmpSessionState::Ready;
    fold_chat_event(
        &mut transcript,
        &ChatEvent::Agent {
            model: None,
            thinking_level: None,
            mode: None,
            omp_session: Some(state.clone()),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::Agent {
            model: None,
            thinking_level: None,
            mode: None,
            omp_session: None,
        },
    );
    assert_eq!(transcript.omp_session, Some(state));
}

#[test]
fn schedule_set_inserts_a_new_row() {
    let mut transcript = transcript();
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-1", 100),
        },
    );
    assert_eq!(transcript.schedules, vec![schedule("s-1", 100)]);
}

#[test]
fn schedule_set_upserts_in_place_by_id() {
    let mut transcript = transcript();
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-1", 100),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-1", 200),
        },
    );
    assert_eq!(transcript.schedules, vec![schedule("s-1", 200)]);
}

#[test]
fn schedule_removed_drops_the_row() {
    let mut transcript = transcript();
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-1", 100),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleRemoved {
            id: "s-1".to_owned(),
        },
    );
    assert!(transcript.schedules.is_empty());
}

#[test]
fn schedule_removed_leaves_other_rows_untouched() {
    let mut transcript = transcript();
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-1", 100),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleSet {
            schedule: schedule("s-2", 200),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::ScheduleRemoved {
            id: "s-1".to_owned(),
        },
    );
    assert_eq!(transcript.schedules, vec![schedule("s-2", 200)]);
}
#[test]
fn omp_commands_replace_catalog_and_interactions_resolve_by_id() {
    use crate::wire::agent_chat::{OmpInteraction, OmpSlashCommand};
    use serde_json::json;

    let mut transcript = transcript();
    let command = |name: &str| OmpSlashCommand {
        name: name.to_owned(),
        description: String::new(),
        input_hint: None,
    };
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpCommands {
            commands: vec![command("first")],
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpCommands {
            commands: vec![command("replacement")],
        },
    );
    assert_eq!(transcript.omp_commands, vec![command("replacement")]);

    let interaction = OmpInteraction {
        id: "request-1".to_owned(),
        request: json!({"kind":"form","requestedSchema":{"properties":{"answer":{"type":"string"}}}}),
    };
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpInteraction {
            interaction: interaction.clone(),
        },
    );
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpInteractionResolved {
            id: interaction.id.clone(),
        },
    );
    assert!(transcript.omp_interactions.is_empty());
}

#[test]
fn omp_ui_reset_assigns_the_live_transport_and_close_clears_it() {
    let mut transcript = transcript();
    transcript.omp_ui = Some(Default::default());
    transcript.omp_ui_error = Some("old diagnostic".into());

    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUiReset {
            transport_id: Some("nonce-a".into()),
        },
    );
    assert_eq!(
        transcript.omp_ui_transport_id.as_deref(),
        Some("nonce-a"),
        "an open must persist the worker-generated native token"
    );
    assert!(transcript.omp_ui.is_none());
    assert!(transcript.omp_ui_error.is_none());

    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUiReset { transport_id: None },
    );
    assert_eq!(
        transcript.omp_ui_transport_id, None,
        "a close must leave no token an old action could still name"
    );
}

#[test]
fn omp_ui_frames_apply_and_a_rejected_frame_records_a_diagnostic() {
    let mut transcript = transcript();
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUi {
            message: serde_json::json!({
                "verb": "o",
                "body": {"id": "surface-a", "mode": "inline"},
            }),
        },
    );
    assert!(
        transcript
            .omp_ui
            .as_ref()
            .is_some_and(|document| document.surfaces.contains_key("surface-a"))
    );
    assert!(transcript.omp_ui_error.is_none());

    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUi {
            message: serde_json::json!({"verb": "unsupported", "body": {}}),
        },
    );
    assert!(transcript.omp_ui_error.is_some());
    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUi {
            message: serde_json::json!({
                "verb": "f",
                "body": {"sf": "surface-a", "s": 1, "ops": []},
            }),
        },
    );
    assert!(
        transcript.omp_ui_error.is_none(),
        "a valid frame clears the previous diagnostic"
    );
}

#[test]
fn omp_ui_transient_requests_never_survive_a_transport_reset() {
    let mut transcript = transcript();
    for message in [
        serde_json::json!({"verb":"o","body":{"id":"surface-a","mode":"inline"}}),
        serde_json::json!({"verb":"f","body":{"sf":"surface-a","s":1,"ops":[
            ["add","body","surface-a",null,{"id":"body","k":"col"}]
        ]}}),
        serde_json::json!({"verb":"f","body":{"sf":"surface-a","s":2,"ops":[
            ["reveal","body","nearest"]
        ]}}),
    ] {
        fold_chat_event(&mut transcript, &ChatEvent::OmpUi { message });
    }
    let pending = transcript
        .omp_ui
        .as_ref()
        .and_then(|document| document.surfaces.get("surface-a"))
        .map_or(0, |surface| surface.intents().len());
    assert_eq!(pending, 1);

    fold_chat_event(
        &mut transcript,
        &ChatEvent::OmpUiReset {
            transport_id: Some("nonce-b".into()),
        },
    );
    assert!(transcript.omp_ui.is_none());

    for message in [
        serde_json::json!({"verb":"o","body":{"id":"surface-a","mode":"inline"}}),
        serde_json::json!({"verb":"f","body":{"sf":"surface-a","s":1,"ops":[
            ["add","body","surface-a",null,{"id":"body","k":"col"}]
        ]}}),
        serde_json::json!({"verb":"f","body":{"sf":"surface-a","s":2,"ops":[
            ["reveal","body","nearest"], ["del","missing"]
        ]}}),
    ] {
        fold_chat_event(&mut transcript, &ChatEvent::OmpUi { message });
    }
    assert!(transcript.omp_ui_error.is_some());
    let pending = transcript
        .omp_ui
        .as_ref()
        .and_then(|document| document.surfaces.get("surface-a"))
        .map_or(0, |surface| surface.intents().len());
    assert_eq!(
        pending, 0,
        "a rejected frame appends no request and a reset kept none"
    );
}
