//! Exercises the mounted chat's response to a worker startup failure.
//! The real transcript fold and Dioxus renderer carry the error to the user.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::platform::connect::CoordRpc;
use dioxus::core::Mutation;
use roost_client_core::{ClientCore, ClientEvent, client::agent_chat::AgentChatIntent};
use roost_protocol::wire::agent_chat::{
    AgentBackend, AgentRunState, ChatEvent, OmpSessionState, Transcript,
};

thread_local! {
    static TEST_PUMP: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

fn root() -> Element {
    let pump = use_hook(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab"))),
            Signal::new(0),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        pump.core()
            .borrow_mut()
            .store_mut()
            .agent_chat
            .load_snapshot(
                "conversation".into(),
                1,
                0,
                Transcript {
                    items: vec![],
                    run_state: AgentRunState::Idle,
                    error: None,
                    model: None,
                    thinking_level: None,
                    mode: None,
                    usage: Default::default(),
                    queued: vec![],
                    omp_commands: vec![],
                    omp_interactions: vec![],
                    schedules: vec![],
                    omp_session: None,
                    omp_ui: None,
                    omp_ui_transport_id: None,
                    omp_ui_error: None,
                },
                0,
            );
        TEST_PUMP.with(|stored| *stored.borrow_mut() = Some(pump.clone()));
        pump
    });
    let conversation = ConversationSummary {
        id: "conversation".into(),
        backend: AgentBackend::Omp,
        title: String::new(),
        worker_fp: "worker".into(),
        worker_label: "worker".into(),
        cwd: "/tmp".into(),
        model: None,
        effective_model: None,
        thinking_level: None,
        mode: "normal".into(),
        parent_id: None,
        agent: None,
        advisor: false,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 0,
        updated_ms: 0,
    };
    rsx! { OmpUiMount { conversation, active: true, host_connected: true, pump } }
}

fn text(mutations: &dioxus::core::Mutations) -> String {
    mutations
        .edits
        .iter()
        .filter_map(|edit| match edit {
            Mutation::CreateText { value } | Mutation::SetText { value } => Some(value.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn startup_capability_failure_replaces_connecting_with_the_worker_error() {
    let mut dom = VirtualDom::new(root);
    let initial = dom.rebuild_to_vec();
    assert!(text(&initial).contains("Connecting to OMP"));
    let pump = TEST_PUMP.with(|stored| stored.borrow_mut().take()).unwrap();
    dom.in_runtime(|| {
        pump.dispatch(ClientEvent::AgentChat(AgentChatIntent::EventsLoaded {
            conversation_id: "conversation".into(),
            batches: vec![(1, vec![ChatEvent::Agent {
                model: None, thinking_level: None, mode: None,
                omp_session: Some(OmpSessionState::Failed {
                    error: "This worker requires the OMP fork with semantic command UI version 1 (tsp).".into(),
                }),
            }])],
            truncated: false,
        }));
    });
    let update = dom.render_immediate_to_vec();
    let visible = text(&update);
    assert!(visible.contains("requires the OMP fork"), "{visible}");
    assert!(visible.contains("OMP view is not connected"), "{visible}");
    assert!(!visible.contains("Connecting to OMP"), "{visible}");
}
