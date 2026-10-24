//! The agent workspace's inspector/chat split and compact inspector drawer.
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
use crate::components::overlay_generation::OverlayGeneration;
use crate::platform::LocalStorageKeyValueStore;
use crate::pump::Pump;
use libero::hooks::{DrawerOptions, ModalScope, use_drawer};
use libero::{
    components::{ActionIcon, Box, Flex},
    sx::sx,
};

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
    let root_sx = use_hook(|| {
        sx().display("flex")
            .height("100%")
            .min_height("0")
            .min_width("0")
            .flex("1")
            .overflow("hidden")
            .background("surface")
            .color("ink")
    });
    let inspector_sx = use_hook(|| {
        sx().display("flex")
            .height("100%")
            .width("20rem")
            .flex_shrink("0")
            .flex_direction("column")
            .overflow("hidden")
            .with("border-right", "1px solid var(--border)")
            .background("var(--lsx-paper-background)")
    });
    let rail_sx = use_hook(|| {
        sx().display("flex")
            .flex_shrink("0")
            .flex_direction("column")
            .align_items("center")
            .gap("xs")
            .with("border-right", "1px solid var(--border)")
            .background("var(--lsx-paper-background)")
            .padding_inline("xs")
            .padding_block("md")
    });
    let chat_sx = use_hook(|| {
        sx().display("flex")
            .min_height("0")
            .min_width("0")
            .flex("1")
            .flex_direction("column")
    });
    let header_sx = use_hook(|| {
        sx().display("flex")
            .min_width("0")
            .align_items("center")
            .gap("sm")
            .with("border-bottom", "1px solid var(--border)")
            .padding_inline("lg")
            .padding_block("md")
    });
    let header_content_sx = use_hook(|| sx().min_width("0").flex("1"));
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
    let fence = use_hook(|| Rc::new(RefCell::new(OverlayGeneration::default())));
    let previous_open = use_hook(|| Rc::new(RefCell::new(None::<bool>)));
    let previous_conversation_id = use_hook(|| Rc::new(RefCell::new(conversation.id.clone())));
    let drawer_conversation = conversation.clone();
    let drawer_transcript = transcript.clone();
    let drawer_catalog = catalog.clone();
    let drawer_pump = pump.clone();
    let drawer_selected_tab = selected_tab;
    let drawer_on_tab_change = on_tab_change;
    let drawer_close = close;
    let drawer = use_drawer(
        DrawerOptions {
            anchor: "end".into(),
            size: "md".into(),
            title: Some("Agent inspector".into()),
            close_button: false,
            ..Default::default()
        },
        move |_scope: ModalScope<()>| {
            let conversation = drawer_conversation.clone();
            let transcript = drawer_transcript.clone();
            let catalog = drawer_catalog.clone();
            let pump = drawer_pump.clone();
            let on_close = drawer_close;
            let on_tab_change = drawer_on_tab_change;
            rsx! {
                AgentInspector {
                    conversation, transcript, catalog, pump,
                    on_close, selected_tab: drawer_selected_tab(), on_tab_change,
                }
            }
        },
    );
    let sync_drawer = drawer;
    let sync_fence = Rc::clone(&fence);
    let sync_previous = Rc::clone(&previous_open);
    let sync_previous_conversation = Rc::clone(&previous_conversation_id);
    use_effect(use_reactive(
        (&overlay, &conversation.id),
        move |(overlay, conversation_id)| {
            let is_open = overlay();
            let was_open = *sync_previous.borrow();
            let identity_changed = *sync_previous_conversation.borrow() != conversation_id;
            *sync_previous_conversation.borrow_mut() = conversation_id;
            let reopen = is_open && was_open == Some(true) && identity_changed;
            if was_open == Some(is_open) && !reopen {
                return;
            }
            *sync_previous.borrow_mut() = Some(is_open);
            if is_open {
                if reopen {
                    sync_fence.borrow_mut().invalidate();
                    sync_drawer.close();
                }
                let generation = sync_fence.borrow_mut().begin();
                let completion_fence = Rc::clone(&sync_fence);
                let mut overlay = overlay;
                sync_drawer.open().onresult(move |_| {
                    if completion_fence.borrow_mut().finish(generation) {
                        overlay.set(false);
                    }
                });
            } else {
                sync_fence.borrow_mut().invalidate();
                sync_drawer.close();
            }
        },
    ));
    use_drop(move || fence.borrow_mut().invalidate());
    rsx! {
        Box {
            component: "section",
            sx: root_sx,
            "data-inspector-inline": inline(),
            "data-testid": "agent-workspace",
            onmounted: move |event: MountedEvent| {
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
                Box {
                    component: "aside",
                    sx: inspector_sx,
                    "aria-label": "Agent inspector",
                    AgentInspector {
                        conversation: conversation.clone(), transcript: transcript.clone(),
                        catalog: catalog.clone(), pump: pump.clone(), on_close: close,
                        selected_tab: selected_tab(), on_tab_change,
                    }
                }
            } else if inline() {
                Box { component: "aside", sx: rail_sx, "aria-label": "Inspector navigation",
                    ActionIcon {
                        icon: pictogram_icons_lucide::panel_left::outlined,
                        aria_label: "Expand agent inspector",
                        title: "Expand inspector",
                        variant: "standard",
                        size: "sm",
                        onclick: show,
                    }
                    for (key, label, icon) in INSPECTOR_TABS {
                        ActionIcon {
                            icon,
                            aria_label: format!("Open {label} inspector"),
                            title: label,
                            variant: "standard",
                            size: "sm",
                            onclick: move |_| { selected_tab.set(key.to_owned()); expanded.set(true); }
                        }
                    }
                }
            }
            Flex { direction: "column", sx: chat_sx,
                Flex { direction: "row", sx: header_sx,
                    Box { component: "div", sx: header_content_sx, {header} }
                    if !inline() {
                        ActionIcon {
                            icon: pictogram_icons_lucide::panel_left::outlined,
                            aria_label: "Open agent inspector",
                            title: "Inspector",
                            variant: "standard",
                            size: "sm",
                            "data-testid": "agent-inspector-toggle",
                            onclick: show,
                        }
                    }
                }
                {children}
            }
        }
    }
}
