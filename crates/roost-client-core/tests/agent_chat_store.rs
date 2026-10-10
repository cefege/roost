//! Agent chat hydration and ordered transcript replica behavior.

use roost_client_core::client::agent_chat::AgentChatState;
use roost_protocol::wire::agent_chat::{
    AgentRunState, ChatEvent, ConversationSummary, Transcript, UsageTotals,
};

fn conversation(id: &str, cwd: &str, updated_ms: i64) -> ConversationSummary {
    ConversationSummary {
        id: id.into(),
        title: "New".into(),
        worker_fp: "worker".into(),
        worker_label: "worker".into(),
        cwd: cwd.into(),
        model: None,
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
        usage: UsageTotals::default(),
    }
}

#[test]
fn hydration_and_frames_replace_snapshots_and_enforce_sequence() {
    let mut state = AgentChatState::default();
    state.hydrate(vec![conversation("a", "/repo", 4)], true);
    assert!(state.host_connected);
    assert!(state.conversations.contains_key("a"));
    state.load_snapshot("a".into(), 3, transcript());
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
    state.load_snapshot("known".into(), 0, transcript());
    state.apply_events("known", 1, &[]);
    assert!(!state.transcripts["known"].stale);
}
