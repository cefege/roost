//! Actual elapsed, picker and card samples advance without another source frame.
//! These tests consume the canonical replica and retained presentation clock.
//! Source stopping or replacing a sample removes its previous running origin.
use super::*;

#[test]
fn cached_negative_countdown_advances_to_zero_and_holds_until_source_reset() {
    let mut state = started(vec![
        json!({"id":"clock","k":"elapsed","p":{"age":-2500,"format":"short"}}),
    ]);
    assert_eq!(elapsed(&state, 1_500), -2_000.0);
    assert_eq!(elapsed(&state, 5_000), 0.0);
    assert!(read(&state, "clock", OmpTimerKind::Elapsed, 5_000).countdown);
    state.apply_events(
        "chat",
        2,
        &[frame(2, json!([["set","label",{"text":"unrelated"}]]))],
        5_500,
    );
    assert_eq!(elapsed(&state, 5_600), 0.0);
    state.apply_events(
        "chat",
        3,
        &[frame(3, json!([["set","clock",{"age":-5000}]]))],
        6_000,
    );
    assert_eq!(elapsed(&state, 6_500), -4_500.0);
}

#[test]
fn picker_numeric_ages_run_but_source_formatted_settlement_removes_the_clock() {
    let picker = |time: Value| {
        json!({"id":"picker","k":"picker","p":{
            "columns":[{"id":"time","format":"elapsed"}],
            "items":[{"id":"agent","label":"Agent","facts":{"time":time}}]
        }})
    };
    let mut state = started(vec![picker(json!(1200))]);
    let fact = |state: &AgentChatState, now| {
        state.transcripts["chat"]
            .presentation_clocks
            .fact("s", "picker", "agent", "time", now)
    };
    state.apply_events(
        "chat",
        2,
        &[frame(2, json!([["set","picker",{"query":"Agent"}]]))],
        2_000,
    );
    assert_eq!(fact(&state, 3_000).unwrap().milliseconds, 3_200.0);
    state.apply_events(
        "chat",
        3,
        &[frame(
            3,
            json!([["set", "picker", picker(json!(1200))["p"]]]),
        )],
        3_500,
    );
    assert_eq!(fact(&state, 4_000).unwrap().milliseconds, 4_200.0);
    state.apply_events(
        "chat",
        4,
        &[frame(
            4,
            json!([["set", "picker", picker(json!("1.2s"))["p"]]]),
        )],
        4_500,
    );
    assert!(fact(&state, 5_000).is_none());
    state.apply_events(
        "chat",
        5,
        &[frame(
            5,
            json!([["set", "picker", picker(json!(1200))["p"]]]),
        )],
        5_000,
    );
    assert_eq!(fact(&state, 5_500).unwrap().milliseconds, 1_700.0);
}

#[test]
fn running_tool_agent_and_retry_clock_fields_share_the_accepted_origin() {
    let mut state = started(vec![
        json!({"id":"tool","k":"tool","p":{"status":"running","age":500}}),
        json!({"id":"agent","k":"agent","p":{"name":"Agent","status":"running",
            "tool":{"name":"read","age":300},"stats":{"age":700},
            "retry":{"attempt":1,"max":3,"age":1000,"delay":5000}}}),
    ]);
    assert_eq!(
        read(&state, "tool", OmpTimerKind::ToolAge, 2_000).milliseconds,
        1_500.0
    );
    assert_eq!(
        read(&state, "agent", OmpTimerKind::AgentTool, 2_000).milliseconds,
        1_300.0
    );
    assert_eq!(
        read(&state, "agent", OmpTimerKind::AgentStats, 2_000).milliseconds,
        1_700.0
    );
    assert_eq!(
        read(&state, "agent", OmpTimerKind::AgentRetry, 2_000).milliseconds,
        -3_000.0
    );
    state.apply_events("chat",2,&[frame(2,json!([
        ["set","tool",{"status":"done"}],
        ["set","agent",{"status":"done","stats":{"age":700,"took":1750},"tool":null,"retry":null}]
    ]))],2_500);
    assert_eq!(
        read(&state, "tool", OmpTimerKind::ToolAge, 5_000).milliseconds,
        500.0
    );
    assert_eq!(
        read(&state, "agent", OmpTimerKind::AgentStats, 5_000).milliseconds,
        1_750.0
    );
    assert!(!read(&state, "agent", OmpTimerKind::AgentStats, 5_000).running);
    assert!(
        state.transcripts["chat"]
            .presentation_clocks
            .timer("s", "agent", OmpTimerKind::AgentRetry, 5_000)
            .is_none()
    );
}
