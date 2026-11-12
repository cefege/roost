//! Model role precedence for native conversation summaries.
//! Kept outside the wire types to keep their declaration file bounded.
//! The conversation fixture explicitly selects the native backend.

use super::{AgentBackend, AgentRunState, ConversationSummary};

fn conversation(mode: &str, agent: Option<&str>) -> ConversationSummary {
    ConversationSummary {
        id: "c".into(),
        backend: AgentBackend::Native,
        title: String::new(),
        worker_fp: String::new(),
        worker_label: String::new(),
        cwd: String::new(),
        model: None,
        effective_model: None,
        thinking_level: None,
        mode: mode.into(),
        parent_id: None,
        agent: agent.map(Into::into),
        advisor: false,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 0,
        updated_ms: 0,
    }
}

#[test]
fn plan_mode_then_subagent_kind_then_default_decide_the_running_role() {
    assert_eq!(conversation("plan", Some("scout")).running_role(), "plan");
    assert_eq!(conversation("normal", Some("scout")).running_role(), "smol");
    assert_eq!(
        conversation("normal", Some("reviewer")).running_role(),
        "slow"
    );
    assert_eq!(conversation("normal", Some("task")).running_role(), "task");
    assert_eq!(
        conversation("normal", Some("other")).running_role(),
        "default"
    );
    assert_eq!(conversation("normal", None).running_role(), "default");
}
