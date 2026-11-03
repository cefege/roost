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
        schedules: Vec::new(),
        omp_session: None,
    }
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
