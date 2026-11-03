//! Agent chat hydration and ordered transcript replica behavior.

use roost_client_core::client::agent_chat::AgentChatState;
use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, ConversationSummary, Transcript, UsageTotals,
};

fn conversation(id: &str, cwd: &str, updated_ms: i64) -> ConversationSummary {
    ConversationSummary {
        backend: roost_protocol::wire::agent_chat::AgentBackend::Native,
        id: id.into(),
        title: "New".into(),
        worker_fp: "worker".into(),
        worker_label: "worker".into(),
        cwd: cwd.into(),
        mode: "normal".into(),
        parent_id: None,
        agent: None,
        advisor: false,
        model: None,
        effective_model: None,
        thinking_level: None,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 1,
        updated_ms,
    }
}
fn transcript() -> Transcript {
    Transcript {
        items: vec![],
        run_state: AgentRunState::Idle,
        error: None,
        model: None,
        thinking_level: None,
        mode: None,
        usage: UsageTotals::default(),
        queued: Vec::new(),
        schedules: Vec::new(),
        omp_session: None,
        omp_commands: Vec::new(),
        omp_interactions: Vec::new(),
    }
}

#[test]
fn hydration_and_frames_replace_snapshots_and_enforce_sequence() {
    let mut state = AgentChatState::default();
    state.hydrate(vec![conversation("a", "/repo", 4)], true);
    assert!(state.host_connected);
    assert!(state.conversations.contains_key("a"));
    state.load_snapshot("a".into(), 0, 3, transcript());
    state.apply_events("a", 4, &[]);
    assert_eq!(state.transcripts["a"].seq, 4);
    state.apply_events("a", 4, &[]);
    assert_eq!(state.transcripts["a"].seq, 4);
    state.apply_events("a", 6, &[]);
    assert!(state.transcripts["a"].stale);
    state.hydrate(vec![conversation("a", "/repo", 5)], false);
    assert!(state.transcripts["a"].stale);
    state.apply_conversation_frame("a".into(), true, None, false);
    assert!(!state.conversations.contains_key("a"));
}

#[test]
fn only_the_host_status_frame_changes_host_connectivity() {
    let mut state = AgentChatState::default();
    state.hydrate(vec![conversation("a", "/repo", 4)], true);
    // A conversation upsert carries no host state; its zero flag is not "offline".
    state.apply_conversation_frame(
        "a".into(),
        false,
        Some(conversation("a", "/repo", 9)),
        false,
    );
    assert!(state.host_connected);
    assert_eq!(state.conversations["a"].updated_ms, 9);
    state.apply_conversation_frame(String::new(), false, None, false);
    assert!(!state.host_connected);
    state.apply_conversation_frame(String::new(), false, None, true);
    assert!(state.host_connected);
}

#[test]
fn snapshot_loaded_transcript_accepts_chat_events_only_when_loaded() {
    let mut state = AgentChatState::default();
    state.apply_events(
        "unknown",
        1,
        &[ChatEvent::RunState {
            run_state: AgentRunState::Running,
            error: None,
        }],
    );
    state.load_snapshot("known".into(), 0, 0, transcript());
    state.apply_events("known", 1, &[]);
    assert!(!state.transcripts["known"].stale);
}

#[test]
fn event_repair_applies_only_contiguous_batches_and_rejects_stale_snapshots() {
    let mut state = AgentChatState::default();
    state.load_snapshot("chat".into(), 1, 5, transcript());
    state.request_snapshot("chat", 3);
    state.apply_event_batches(
        "chat",
        &[(
            6,
            vec![ChatEvent::RunState {
                run_state: AgentRunState::Running,
                error: None,
            }],
        )],
        false,
    );
    assert_eq!(state.transcripts["chat"].seq, 6);
    assert!(!state.transcripts["chat"].stale);

    state.load_snapshot("chat".into(), 2, 4, transcript());
    assert_eq!(state.transcripts["chat"].seq, 6);
    state.apply_event_batches("chat", &[(8, vec![])], false);
    assert!(state.transcripts["chat"].stale);
    state.apply_event_batches("chat", &[], true);
    assert!(state.transcripts["chat"].stale);
}

#[test]
fn omp_config_refresh_invalidates_choices_without_changing_the_selected_model() {
    let agent = ChatEvent::Agent {
        model: None,
        thinking_level: Some("high".into()),
        mode: None,
        omp_session: Some(roost_protocol::wire::agent_chat::OmpSessionState::Ready),
    };
    for repair_batch in [false, true] {
        let mut state = AgentChatState::default();
        state.load_snapshot("chat".into(), 0, 0, transcript());
        let apply = |state: &mut AgentChatState, seq, event: ChatEvent| {
            if repair_batch {
                state.apply_event_batches("chat", &[(seq, vec![event])], false);
            } else {
                state.apply_events("chat", seq, &[event]);
            }
        };
        apply(&mut state, 1, agent.clone());
        let before = state.transcripts["chat"].transcript.clone();
        apply(&mut state, 2, agent.clone());
        assert_eq!(state.transcripts["chat"].transcript, before);
        assert_eq!(state.transcripts["chat"].settings_seq, 2);

        apply(
            &mut state,
            3,
            ChatEvent::Usage {
                usage: UsageTotals::default(),
            },
        );
        assert_eq!(state.transcripts["chat"].settings_seq, 2);
        apply(&mut state, 5, agent.clone());
        assert!(state.transcripts["chat"].stale);
        assert_eq!(state.transcripts["chat"].settings_seq, 2);
    }
}
