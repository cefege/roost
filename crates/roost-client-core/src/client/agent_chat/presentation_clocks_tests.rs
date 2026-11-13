//! Consumer-visible timing across canonical replica admission and recovery.
//! No local renderer mount or unrelated UI frame may restart a source timer.
//! Countdown samples come from the native cached CountdownTimer descriptor.
use super::{OmpTimerKind, OmpTimerReading};
use crate::client::agent_chat::AgentChatState;
use roost_protocol::wire::agent_chat::{AgentRunState, ChatEvent, Transcript, UsageTotals};
use serde_json::{Value, json};

#[path = "presentation_countdown_tests.rs"]
mod countdown;
#[path = "presentation_recovery_tests.rs"]
mod recovery;

fn empty() -> Transcript {
    Transcript {
        items: vec![],
        run_state: AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        mode: None,
        usage: UsageTotals::default(),
        queued: vec![],
        schedules: vec![],
        omp_session: None,
        omp_commands: vec![],
        omp_interactions: vec![],
        omp_ui: None,
        omp_ui_transport_id: None,
        omp_ui_error: None,
    }
}

fn ui(message: Value) -> ChatEvent {
    ChatEvent::OmpUi { message }
}
fn open() -> ChatEvent {
    ui(json!({"verb":"o","body":{"id":"s","mode":"inline"}}))
}
fn frame(sequence: u64, ops: Value) -> ChatEvent {
    ui(json!({"verb":"f","body":{"sf":"s","s":sequence,"ops":ops}}))
}
fn initial(nodes: Vec<Value>) -> Vec<ChatEvent> {
    let mut ops = vec![
        json!(["add","main","s",null,{"id":"main","k":"col"}]),
        json!(["add","dock","s",null,{"id":"dock","k":"col"}]),
        json!(["add","layer","s",null,{"id":"layer","k":"col"}]),
        json!(["add","label","main",null,{"id":"label","k":"text","p":{"text":"initial"}}]),
    ];
    for node in nodes {
        ops.push(json!(["add", node["id"], "main", null, node]));
    }
    vec![
        ChatEvent::OmpUiReset {
            transport_id: Some("transport-a".into()),
        },
        open(),
        frame(1, ops.into()),
    ]
}
fn started(nodes: Vec<Value>) -> AgentChatState {
    let mut state = AgentChatState::default();
    state.load_snapshot("chat".into(), 0, 0, empty(), 0);
    state.apply_events("chat", 1, &initial(nodes), 1_000);
    assert!(state.transcripts["chat"].transcript.omp_ui_error.is_none());
    state
}
fn read(state: &AgentChatState, id: &str, kind: OmpTimerKind, now: u64) -> OmpTimerReading {
    state.transcripts["chat"]
        .presentation_clocks
        .timer("s", id, kind, now)
        .expect("accepted timer")
}
fn elapsed(state: &AgentChatState, now: u64) -> f64 {
    read(state, "clock", OmpTimerKind::Elapsed, now).milliseconds
}

#[test]
fn cached_elapsed_ignores_unrelated_refresh_duplicate_gap_and_rejected_frame() {
    let mut state = started(vec![json!({"id":"clock","k":"elapsed","p":{"age":1200}})]);
    assert_eq!(elapsed(&state, 1_500), 1_700.0);
    state.apply_events(
        "chat",
        2,
        &[frame(
            2,
            json!([["set","label",{"text":"edit acknowledged"}]]),
        )],
        1_800,
    );
    state.apply_events(
        "chat",
        3,
        &[frame(3, json!([["set","clock",{"age":1200}]]))],
        2_200,
    );
    assert_eq!(elapsed(&state, 2_400), 2_600.0);
    state.apply_events(
        "chat",
        3,
        &[frame(4, json!([["set","clock",{"age":9000}]]))],
        2_600,
    );
    let clocks = state.transcripts["chat"].presentation_clocks.clone();
    state.apply_events(
        "chat",
        4,
        &[frame(
            4,
            json!([["set","clock",{"age":9000}],["del","missing"]]),
        )],
        3_100,
    );
    assert_eq!(state.transcripts["chat"].presentation_clocks, clocks);
    assert_eq!(elapsed(&state, 3_200), 3_400.0);
    state.apply_events("chat", 5, &[frame(3, json!([]))], 3_500);
    assert_eq!(elapsed(&state, 3_600), 3_800.0);
    state.apply_events(
        "chat",
        7,
        &[frame(4, json!([["set","clock",{"age":9000}]]))],
        4_000,
    );
    assert!(state.transcripts["chat"].stale);
    assert_eq!(elapsed(&state, 4_000), 4_200.0);
}

#[test]
fn source_age_stop_and_resume_rebase_only_their_own_timer() {
    let mut state = started(vec![json!({"id":"clock","k":"elapsed","p":{"age":1000}})]);
    state.apply_events(
        "chat",
        2,
        &[frame(2, json!([["set","clock",{"stopped":1750}]]))],
        2_500,
    );
    assert_eq!(elapsed(&state, 5_000), 1_750.0);
    assert!(!read(&state, "clock", OmpTimerKind::Elapsed, 5_000).running);
    state.apply_events(
        "chat",
        3,
        &[frame(
            3,
            json!([["set","clock",{"age":1000,"stopped":null}]]),
        )],
        6_000,
    );
    assert_eq!(elapsed(&state, 6_100), 1_100.0);
    assert!(read(&state, "clock", OmpTimerKind::Elapsed, 6_100).running);
    state.apply_events(
        "chat",
        4,
        &[frame(4, json!([["set","clock",{"age":1900}]]))],
        7_000,
    );
    assert_eq!(elapsed(&state, 7_050), 1_950.0);
}
