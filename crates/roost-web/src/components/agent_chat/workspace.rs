//! The agent workspace's inspector/chat split and compact inspector sheet.
//! Mounted by `surface`; measures its own pane so splits never squeeze the
//! chat behind a desktop-width inspector. The deck routes inspector requests
//! here by conversation ID.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::KeyValueStore;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog, Transcript};

use super::inspector::{AgentInspector, INSPECTOR_TABS};
use crate::components::deck::deck_dom::{SizeWatch, client_size};
use crate::components::layout::portal::Portal;
use crate::components::ui::{Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle};
use crate::platform::LocalStorageKeyValueStore;
use crate::pump::Pump;

const INSPECTOR_TAB_PREFERENCE: &str = "roost.agent.inspector.tab";
const INLINE_INSPECTOR_MIN_WIDTH: f64 = 960.0;

#[component]
pub(super) fn AgentWorkspace(
    conversation: ConversationSummary,
    transcript: Transcript,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    active: bool,
    children: Element,
) -> Element {
    let storage = use_hook(|| Rc::new(LocalStorageKeyValueStore::new()));
    let mut expanded = use_signal(|| false);
    let mut selected_tab = use_signal(|| {
        storage
            .get(INSPECTOR_TAB_PREFERENCE)
            .filter(|value| {
                INSPECTOR_TABS
                    .iter()
                    .any(|(key, _, _)| *key == value.as_str())
            })
            .unwrap_or_else(|| "session".to_owned())
    });
    let mut overlay = use_signal(|| false);
    let mut inline = use_signal(|| false);
    let observer: Rc<RefCell<Option<SizeWatch>>> = use_hook(Rc::default);
    let tab_storage = Rc::clone(&storage);
    use_effect(move || tab_storage.set(INSPECTOR_TAB_PREFERENCE, &selected_tab()));
    let on_tab_change = EventHandler::new(move |tab: String| selected_tab.set(tab));
    let inspector_request =
        try_consume_context::<crate::components::deck::terminal_deck::AgentInspectorRequest>();
    let requested_conversation_id = conversation.id.clone();
    use_effect(move || {
        let Some(mut request) = inspector_request else {
            return;
        };
        if request.0.read().as_deref() == Some(requested_conversation_id.as_str()) {
            if inline() {
                expanded.set(!expanded());
            } else {
                overlay.set(!overlay());
            }
            request.0.set(None);
        }
    });
    use_effect(use_reactive((&active,), move |(active,)| {
        if inline() || !active {
            overlay.set(false);
        }
    }));
    let close = EventHandler::new(move |()| {
        if inline() {
            expanded.set(false);
        } else {
            overlay.set(false);
        }
    });
    rsx! {
        section {
            class: "flex h-full min-h-0 min-w-0 flex-1 overflow-hidden bg-background text-foreground",
            "data-testid": "agent-workspace",
            "data-inspector-inline": inline(),
            onmounted: move |event| {
                let mounted = event.data();
                let measure_node = Rc::clone(&mounted);
                let mut measure = move || {
                    if let Some((width, _)) = client_size(&measure_node) {
                        let next = width >= INLINE_INSPECTOR_MIN_WIDTH;
                        if *inline.peek() != next {
                            inline.set(next);
                        }
                    }
                };
                measure();
                let watch = SizeWatch::new(measure);
                if let Some(watch) = watch.as_ref() {
                    watch.watch(&mounted);
                }
                *observer.borrow_mut() = watch;
            },
            if inline() && expanded() {
                aside {
                    class: "flex h-full w-80 shrink-0 flex-col overflow-hidden border-r border-border bg-card",
                    "aria-label": "Agent inspector",
                    AgentInspector {
                        conversation: conversation.clone(), transcript: transcript.clone(),
                        catalog: catalog.clone(), pump: pump.clone(), on_close: close,
                        selected_tab: selected_tab(), on_tab_change,
                    }
                }
            }
            div { class: "flex min-h-0 min-w-0 flex-1 flex-col",
                {children}
            }
            // The deck's pane stacking context must not trap the mobile modal.
            if active && !inline() {
                Portal {
                    Sheet {
                        open: overlay(),
                        on_open_change: move |next| overlay.set(next),
                        SheetContent {
                            class: "flex h-full w-full max-w-md flex-col overflow-hidden p-0",
                            test_id: "agent-inspector-sheet", show_close_button: false,
                            SheetHeader { class: "sr-only",
                                SheetTitle { "Agent inspector" }
                                SheetDescription { "Session, runtime state, tasks, tools and configuration." }
                            }
                            AgentInspector { conversation, transcript, catalog, pump, on_close: close, selected_tab: selected_tab(), on_tab_change }
                        }
                    }
                }
            }
        }
    }
}
