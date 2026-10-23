//! The agent workspace's inspector/chat split and compact inspector sheet.
//! Mounted by `surface`; observes its own pane rather than the window so split
//! panes never squeeze the chat behind a desktop-width inspector. State and
//! actions remain in the native client store and the controlled inspector.

use std::cell::RefCell;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_client_core::KeyValueStore;
use roost_protocol::wire::agent_chat::{ConversationSummary, ModelsCatalog, Transcript};

use super::inspector::{AgentInspector, INSPECTOR_TABS};
use crate::components::deck::deck_dom::{SizeWatch, client_size};
use crate::components::layout::portal::Portal;
use crate::components::ui::{
    ButtonSize, ButtonVariant, IconButton, IconType, Sheet, SheetContent, SheetDescription,
    SheetHeader, SheetTitle,
};
use crate::platform::LocalStorageKeyValueStore;
use crate::pump::Pump;

const INSPECTOR_PREFERENCE: &str = "roost.agent.inspector.expanded";
const INSPECTOR_TAB_PREFERENCE: &str = "roost.agent.inspector.tab";
const INLINE_INSPECTOR_MIN_WIDTH: f64 = 960.0;

#[component]
pub(super) fn AgentWorkspace(
    conversation: ConversationSummary,
    transcript: Transcript,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    active: bool,
    header: Element,
    children: Element,
) -> Element {
    let storage = use_hook(|| Rc::new(LocalStorageKeyValueStore::new()));
    let mut expanded = use_signal(|| storage.get(INSPECTOR_PREFERENCE).as_deref() != Some("false"));
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
    let save_storage = Rc::clone(&storage);
    use_effect(move || {
        save_storage.set(
            INSPECTOR_PREFERENCE,
            if expanded() { "true" } else { "false" },
        )
    });
    let tab_storage = Rc::clone(&storage);
    use_effect(move || tab_storage.set(INSPECTOR_TAB_PREFERENCE, &selected_tab()));
    let on_tab_change = EventHandler::new(move |tab: String| selected_tab.set(tab));
    use_effect(use_reactive((&active,), move |(active,)| {
        if inline() || !active {
            overlay.set(false);
        }
    }));
    let show = move |_| {
        if inline() {
            expanded.set(!expanded());
        } else {
            overlay.set(true);
        }
    };
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
            } else if inline() {
                aside { class: "flex shrink-0 flex-col items-center gap-1 border-r border-border bg-card px-1 py-3", "aria-label": "Inspector navigation",
                    IconButton {
                        icon: IconType::PanelLeft, label: "Expand agent inspector", title: "Expand inspector",
                        variant: ButtonVariant::Ghost, size: ButtonSize::IconSm,
                        onclick: show,
                    }
                    for (key, label, icon) in INSPECTOR_TABS {
                        IconButton { icon, label: format!("Open {label} inspector"), title: label,
                            variant: ButtonVariant::Ghost, size: ButtonSize::IconSm,
                            onclick: move |_| { selected_tab.set(key.to_owned()); expanded.set(true); } }
                    }
                }
            }
            div { class: "flex min-h-0 min-w-0 flex-1 flex-col",
                div { class: "flex min-w-0 items-center gap-2 border-b border-border px-4 py-3",
                    div { class: "min-w-0 flex-1", {header} }
                    if !inline() {
                        IconButton {
                            icon: IconType::PanelLeft, label: "Open agent inspector", title: "Inspector",
                            variant: ButtonVariant::Ghost, size: ButtonSize::IconSm,
                            "data-testid": "agent-inspector-toggle",
                            onclick: show,
                        }
                    }
                }
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
