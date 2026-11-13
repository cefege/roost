//! Retained timer origins survive parking, remount and accepted snapshot repair.
//! Source removal and transport replacement discard old identity-bound origins.
//! The injected host clock dates admission rather than RPC construction.
use super::*;
use crate::client::agent_chat::AgentChatIntent;
use roost_protocol::wire::agent_chat::fold_chat_event;

#[test]
fn hidden_kept_and_remounted_history_preserves_origins_but_prunes_removed_dock() {
    let mut state = started(vec![json!({"id":"clock","k":"elapsed","p":{"age":1000}})]);
    state.apply_events(
        "chat",
        2,
        &[frame(
            2,
            json!([
                ["add","dock-clock","dock",null,{"id":"dock-clock","k":"elapsed","p":{"age":200}}],
                ["set","clock",{"hidden":true}]
            ]),
        )],
        1_500,
    );
    state.apply_events(
        "chat",
        3,
        &[ui(json!({"verb":"x","body":{"id":"s","keep":true}}))],
        2_000,
    );
    assert_eq!(elapsed(&state, 2_400), 2_400.0);
    assert!(
        state.transcripts["chat"]
            .presentation_clocks
            .timer("s", "dock-clock", OmpTimerKind::Elapsed, 2_400)
            .is_none()
    );
    state.apply_events(
        "chat",
        4,
        &[ui(json!({"verb":"o","body":{"id":"s","adopt":true}}))],
        2_500,
    );
    let parked = state.transcripts["chat"].clone();
    assert_eq!(
        parked
            .presentation_clocks
            .timer("s", "clock", OmpTimerKind::Elapsed, 6_000)
            .unwrap()
            .milliseconds,
        6_000.0
    );
    state.load_snapshot("chat".into(), 0, 4, parked.transcript.clone(), 3_000);
    assert_eq!(elapsed(&state, 3_500), 3_500.0);
    state.request_snapshot("chat", 2);
    state.load_snapshot("chat".into(), 1, 5, empty(), 4_000);
    assert_eq!(elapsed(&state, 4_100), 4_100.0);
    let mut snapshot = parked.transcript;
    fold_chat_event(
        &mut snapshot,
        &frame(3, json!([["set","clock",{"age":500}]])),
    );
    state.load_snapshot("chat".into(), 2, 5, snapshot, 5_000);
    assert_eq!(elapsed(&state, 5_500), 1_000.0);
}

#[test]
fn removal_recreation_and_transport_reset_do_not_reuse_a_previous_origin() {
    let node = json!({"id":"clock","k":"elapsed","p":{"age":1000}});
    let mut state = started(vec![node.clone()]);
    state.apply_events(
        "chat",
        2,
        &[
            frame(2, json!([["del", "clock"]])),
            frame(3, json!([["add", "clock", "main", null, node.clone()]])),
        ],
        4_000,
    );
    assert_eq!(elapsed(&state, 4_500), 1_500.0);
    let mut replacement = initial(vec![node]);
    state.apply_events("chat", 3, &replacement, 6_000);
    assert_eq!(elapsed(&state, 6_500), 1_500.0);
    replacement[0] = ChatEvent::OmpUiReset {
        transport_id: Some("transport-b".into()),
    };
    state.apply_events("chat", 4, &replacement, 8_000);
    assert_eq!(elapsed(&state, 8_500), 1_500.0);
    state.apply_events(
        "chat",
        5,
        &[ui(json!({"verb":"x","body":{"id":"s","keep":false}}))],
        9_000,
    );
    assert!(
        state.transcripts["chat"]
            .presentation_clocks
            .timer("s", "clock", OmpTimerKind::Elapsed, 9_000)
            .is_none()
    );
}

#[test]
fn injected_host_clock_dates_snapshot_and_repair_admission_not_rpc_creation() {
    use crate::store::prefs::PrefDefaults;
    use crate::{ClientCore, ClientEvent, ClientStateStore, MemoryClock};
    use std::rc::Rc;
    let clock = Rc::new(MemoryClock::starting_at(5_000));
    let mut core = ClientCore::new(
        clock.clone(),
        Rc::new(ClientStateStore::in_memory()),
        "clock-tab",
        &PrefDefaults::default(),
    );
    let transcript = started(vec![json!({"id":"clock","k":"elapsed","p":{"age":1000}})])
        .transcripts
        .remove("chat")
        .unwrap()
        .transcript;
    core.handle(ClientEvent::AgentChat(AgentChatIntent::SnapshotLoaded {
        conversation_id: "chat".into(),
        generation: 0,
        seq: 1,
        transcript: Box::new(transcript),
    }));
    clock.advance(500);
    assert_eq!(elapsed(&core.store().agent_chat, 5_500), 1_500.0);
    core.handle(ClientEvent::AgentChat(AgentChatIntent::EventsLoaded {
        conversation_id: "chat".into(),
        truncated: false,
        batches: vec![(2, vec![frame(2, json!([["set","clock",{"age":2000}]]))])],
    }));
    assert_eq!(elapsed(&core.store().agent_chat, 6_000), 2_500.0);
}

#[test]
fn identical_surface_and_node_ids_do_not_share_origins_between_conversations() {
    let mut state = started(vec![json!({"id":"clock","k":"elapsed","p":{"age":1000}})]);
    let snapshot = state.transcripts["chat"].transcript.clone();
    state.load_snapshot("second".into(), 0, 1, snapshot, 4_000);
    let second = state.transcripts["second"]
        .presentation_clocks
        .timer("s", "clock", OmpTimerKind::Elapsed, 4_500)
        .unwrap();
    assert_eq!(second.milliseconds, 1_500.0);
    assert_eq!(elapsed(&state, 4_500), 4_500.0);
    state.forget("second");
    assert_eq!(elapsed(&state, 5_000), 5_000.0);
}
