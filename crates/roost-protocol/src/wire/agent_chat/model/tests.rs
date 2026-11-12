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

#[test]
fn backend_wire_and_json_decoding_preserve_archive_defaults() {
    for (wire, expected) in [
        ("native", AgentBackend::Native),
        ("omp", AgentBackend::Omp),
        ("rpi", AgentBackend::Rpi),
        ("", AgentBackend::Native),
        ("unknown", AgentBackend::Native),
    ] {
        assert_eq!(AgentBackend::from_wire(wire), expected);
        assert!(matches!(
            serde_json::from_value::<AgentBackend>(serde_json::json!(wire)),
            Ok(decoded) if decoded == expected
        ));
    }
    for backend in [AgentBackend::Native, AgentBackend::Omp, AgentBackend::Rpi] {
        assert!(matches!(
            serde_json::to_value(backend),
            Ok(encoded) if encoded == serde_json::json!(backend.as_wire())
        ));
        let mut summary = conversation("normal", None);
        summary.backend = backend;
        assert_eq!(summary.is_native(), backend == AgentBackend::Native);
    }
    assert_eq!(AgentBackend::default(), AgentBackend::Native);
}

#[test]
fn stored_conversation_without_backend_is_archived() -> Result<(), serde_json::Error> {
    let mut stored = serde_json::to_value(conversation("normal", None))?;
    let removed = stored
        .as_object_mut()
        .and_then(|fields| fields.remove("backend"));
    assert_eq!(removed, Some(serde_json::json!("native")));
    let decoded: ConversationSummary = serde_json::from_value(stored)?;
    assert!(decoded.is_native());
    Ok(())
}
