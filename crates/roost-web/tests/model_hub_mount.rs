//! The `/models` hub mounted for real: its model rows reach the page, and Enter on
//! a row opens the role-assignment strip with its chips.
//!
//! Keys are delivered through the runtime's own event path (`handle_event`), so
//! the hub's `onkeydown` reads them the way a browser would. Settings reads and
//! writes are wasm-only, so this drives the strip's presence and chips and leaves
//! the write of a chosen role to the unit tests that own `apply_settings`.
//!
//! Test root, so the unwrap allowance is declared here (`CLAUDE.md` "the test
//! exemption reaches a test binary and not a fixture").

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

const ROW_TEST_ID: &str = "agent-model-hub-row-anthropic/claude-test";
const ROLES_VIEW_TEST_ID: &str = "agent-model-hub-view-roles";
const ROWS_TEST_ID: &str = "agent-model-hub-rows";
const STRIP_TEST_ID: &str = "agent-model-hub-strip";
const FIRST_CHIP_TEST_ID: &str = "agent-model-hub-strip-chip-0";
const SECOND_CHIP_TEST_ID: &str = "agent-model-hub-strip-chip-1";

const SETTLE_ROUNDS: usize = 8;
const MAX_VIEW_STEPS: usize = 8;

thread_local! {
    /// The pump the root built, handed back out of the render pass.
    static BUILT: RefCell<Option<Pump>> = const { RefCell::new(None) };
}

fn catalog() -> ModelsCatalog {
    ModelsCatalog {
        models: vec![ModelEntry {
            provider: "anthropic".to_owned(),
            model_id: "claude-test".to_owned(),
            name: "Claude Test".to_owned(),
            available: true,
            ..ModelEntry::default()
        }],
        providers: Vec::<ProviderEntry>::new(),
        thinking_levels: Vec::new(),
        default_model: None,
        recent_models: Vec::new(),
    }
}

fn conversation() -> ConversationSummary {
    ConversationSummary {
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
    }
}

/// The root `VirtualDom::new` wants: the pump the hub reads, and the hub open.
fn hub_root() -> Element {
    let pump = use_hook(|| {
        let pump = Pump::new(
            Rc::new(RefCell::new(ClientCore::in_memory("tab-models-hub"))),
            Signal::new(0_u64),
            Rc::new(CoordRpc::new("http://127.0.0.1:4113", "")),
        );
        BUILT.with(|built| *built.borrow_mut() = Some(pump.clone()));
        pump
    });
    let open = use_signal(|| true);
    let connect_provider = use_signal(|| None::<ProviderEntry>);
    rsx! {
        ModelHub {
            open,
            conversation: conversation(),
            catalog: Some(catalog()),
            pump,
            connect_provider,
        }
    }
}

/// A mounted hub, the pump it reads, and the page it has painted.
struct Hub {
    dom: VirtualDom,
    tree: RenderedTree,
}

impl Hub {
    fn settle(&mut self) {
        for _ in 0..SETTLE_ROUNDS {
            let mutations = self.dom.render_immediate_to_vec();
            self.tree.apply(mutations);
            self.dom.process_events();
        }
    }

    /// One key, delivered to `target` and bubbling the way a browser's would.
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

    fn first(&self, test_id: &str) -> ElementId {
        *self
            .tree
            .carrying(test_id)
            .first()
            .unwrap_or_else(|| panic!("no element carries {test_id} on the page"))
    }

    fn count(&self, test_id: &str) -> usize {
        self.tree.count(test_id)
    }
}

/// The hub as it boots: open on the Roles view, with the catalog and no strip.
fn mounted() -> Hub {
    set_event_converter(Box::new(SerializedHtmlEventConverter));
    let mut dom = VirtualDom::new(hub_root);
    let mut tree = RenderedTree::default();
    let mut initial = Mutations::default();
    dom.rebuild(&mut initial);
    tree.apply(initial);
    let mut hub = Hub { dom, tree };
    hub.settle();
    hub
}

/// Move from Roles to All models with the sidebar keys the hub reads: Left takes
/// focus to the sidebar, Down steps the view until the rows are on the page.
fn show_all_models(hub: &mut Hub) {
    let sidebar_entry = hub.first(ROLES_VIEW_TEST_ID);
    hub.press(sidebar_entry, Key::ArrowLeft);
    for _ in 0..MAX_VIEW_STEPS {
        if hub.count(ROWS_TEST_ID) > 0 {
            return;
        }
        hub.press(sidebar_entry, Key::ArrowDown);
    }
    panic!("the model rows never reached the page after {MAX_VIEW_STEPS} view steps");
}

#[test]
fn the_all_models_view_lists_the_catalog_model() {
    let mut hub = mounted();
    show_all_models(&mut hub);

    assert_eq!(
        hub.count(ROW_TEST_ID),
        1,
        "the catalog's one model is not a row on the All models view, so the hub cannot show \
         or assign it"
    );
}

#[test]
fn enter_on_a_model_row_opens_its_role_strip() {
    let mut hub = mounted();
    let sidebar_entry = hub.first(ROLES_VIEW_TEST_ID);
    show_all_models(&mut hub);
    assert_eq!(
        hub.count(STRIP_TEST_ID),
        0,
        "the role strip is on the page before any model was activated"
    );

    hub.press(sidebar_entry, Key::ArrowRight);
    hub.press(sidebar_entry, Key::Enter);

    assert_eq!(
        hub.count(STRIP_TEST_ID),
        1,
        "Enter on a model row did not open its role-assignment strip, so the model cannot be \
         assigned to any role from the hub"
    );
    assert!(
        hub.count(FIRST_CHIP_TEST_ID) == 1 && hub.count(SECOND_CHIP_TEST_ID) == 1,
        "the strip opened without its chips: the first chip is the chat scope and the rest are \
         the chat roles, so both chip positions must be on the page"
    );
}
