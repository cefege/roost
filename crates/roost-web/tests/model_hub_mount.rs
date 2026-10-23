//! A loaded model catalog does not make an unhydrated settings document writable.
//! The native mount leaves the wasm-only settings request pending.
//! Keyboard actions must not expose role assignments over default settings.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod palette_support;

use std::any::Any;
use std::cell::RefCell;
use std::rc::Rc;

use dioxus::core::{ElementId, Event, Mutations};
use dioxus::html::{
    Code, Key, Location, Modifiers, PlatformEventData, SerializedHtmlEventConverter,
    SerializedKeyboardData, set_event_converter,
};
use dioxus::prelude::*;
use roost_client_core::ClientCore;
use roost_protocol::wire::agent_chat::{
    AgentRunState, ConversationSummary, ModelEntry, ModelsCatalog, ProviderEntry,
};
use roost_web::components::agent_chat::model_hub::ModelHub;
use roost_web::platform::connect::CoordRpc;
use roost_web::pump::Pump;

use palette_support::RenderedTree;

fn hub_root() -> Element {
    let pump = use_hook(|| {
        Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-models-hub"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        )
    });
    let open = use_signal(|| true);
    let connect_provider = use_signal(|| None::<ProviderEntry>);
    let catalog = ModelsCatalog {
        models: vec![ModelEntry {
            provider: "anthropic".to_owned(),
            model_id: "claude-test".to_owned(),
            name: "Claude Test".to_owned(),
            available: true,
            ..ModelEntry::default()
        }],
        providers: Vec::new(),
        thinking_levels: Vec::new(),
        default_model: None,
        recent_models: Vec::new(),
    };
    let conversation = ConversationSummary {
        id: "conversation-1".to_owned(),
        title: "A chat".to_owned(),
        worker_fp: "00000000000000000000000000000000000000000000000000000000000000ff".to_owned(),
        worker_label: "test-worker".to_owned(),
        cwd: "/tmp".to_owned(),
        model: None,
        effective_model: None,
        thinking_level: None,
        mode: "normal".to_owned(),
        parent_id: None,
        agent: None,
        advisor: false,
        run_state: AgentRunState::Idle,
        error: None,
        created_ms: 0,
        updated_ms: 0,
    };
    rsx! {
        ModelHub { open, conversation, catalog: Some(catalog), pump, connect_provider }
    }
}

struct Hub {
    dom: VirtualDom,
    tree: RenderedTree,
}

impl Hub {
    fn settle(&mut self) {
        for _ in 0..8 {
            let mutations = self.dom.render_immediate_to_vec();
            self.tree.apply(mutations);
            self.dom.process_events();
        }
    }

    fn press(&mut self, target: ElementId, key: Key) {
        let serialized = SerializedKeyboardData::new(
            key,
            Code::Unidentified,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        );
        let data: Rc<dyn Any> = Rc::new(PlatformEventData::new(Box::new(serialized)));
        let event: Event<dyn Any> = Event::new(data, true);
        self.dom
            .in_runtime(|| self.dom.runtime().handle_event("keydown", event, target));
        self.settle();
    }
}

#[test]
fn unhydrated_settings_block_keyboard_role_assignment() {
    set_event_converter(Box::new(SerializedHtmlEventConverter));
    let mut dom = VirtualDom::new(hub_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let mut hub = Hub { dom, tree };
    hub.settle();
    let target = *hub
        .tree
        .carrying("agent-model-hub")
        .first()
        .expect("the loading dialog is mounted");
    hub.press(target, Key::ArrowLeft);
    for _ in 0..8 {
        hub.press(target, Key::ArrowDown);
    }
    hub.press(target, Key::ArrowRight);
    hub.press(target, Key::Enter);
    assert_eq!(hub.tree.count("agent-model-hub-strip"), 0);
    assert_eq!(hub.tree.count("agent-model-hub-view-roles"), 0);
}
