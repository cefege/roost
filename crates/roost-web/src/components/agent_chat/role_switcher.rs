//! Model-role popover for the agent composer. It loads settings on each open,
//! lets a role's model and thinking level be edited in place, and persists
//! changes through `settings::agent_roles`.

mod panel;

use panel::RolePanel;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{AgentSettingsView, ConversationSummary, ModelsCatalog};

use super::role_switch::RoleLine;
use crate::components::file_viewer::dom::focus_by_id;
use crate::components::settings::agent_roles::load_settings;
use crate::pump::Pump;
use dioxus::html::input_data::keyboard_types::{Key, NamedKey};
use libero::components::{Box, Button, Text};
use libero::hooks::{PopoverOptions, Side, use_element, use_id, use_popover};
use libero::sx::sx;
use libero::use_theme;
use std::cell::RefCell;
use std::rc::Rc;

/// The model button and the panel it opens: every model role this chat runs,
/// with an editor for each. Owns the open and editing state. Settings load on
/// each open, so a save elsewhere never shows stale here.
#[component]
pub(super) fn RoleSwitcher(
    trigger_id: String,
    trigger_label: String,
    trigger_title: String,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
) -> Element {
    let mut open = use_signal(|| false);
    let mut editing = use_signal(|| None::<RoleLine>);
    let settings = use_signal(AgentSettingsView::default);
    let error = use_signal(String::new);
    let loaded = use_signal(|| false);
    let mut layout_revision = use_signal(|| 0_u64);
    let remeasure = use_callback(move |()| {
        let next = layout_revision.peek().wrapping_add(1);
        layout_revision.set(next);
    });
    use_effect(move || {
        if open() {
            let _loaded = loaded();
            let _settings = settings.read();
            let _error = error.read();
            let _editing = editing.read();
            remeasure.call(());
        }
    });
    let previous_open = use_hook(|| Rc::new(RefCell::new(false)));
    let load_pump = pump.clone();
    use_effect(use_reactive((&open,), move |(is_open,)| {
        let was_open = *previous_open.borrow();
        *previous_open.borrow_mut() = is_open();
        if is_open() && !was_open {
            load_settings(load_pump.clone(), settings, error, loaded);
        }
    }));
    let close = EventHandler::new({
        let mut open = open;
        let mut editing = editing;
        move |_| {
            open.set(false);
            editing.set(None);
        }
    });
    let escape_trigger_id = trigger_id.clone();
    let theme = use_theme();
    let anchor = use_element();
    let trigger_label_sx = use_hook(|| {
        sx().min_width("0")
            .overflow("hidden")
            .with("overscroll-behavior-y", "auto")
            .text_overflow("ellipsis")
            .white_space("nowrap")
    });
    let content_id = use_id();
    let floating_sx = use_hook(|| {
        sx().background("surface")
            .padding("sm")
            .width("20rem")
            .max_height("calc(100dvh - 2rem)")
            .overflow_y("auto")
    });
    let popover = use_popover(
        anchor,
        open(),
        PopoverOptions::new(theme.popover.gap, theme.popover.padding)
            .side(Side::Bottom)
            .remeasure(layout_revision())
            .dismiss(true),
    );
    popover.on_dismiss(move || {
        open.set(false);
        editing.set(None);
    });
    let escape_close = close;
    popover.show(open().then(|| rsx! {
        Box {
            id: "{content_id}",
            role: "dialog",
            "aria-label": "Model roles",
            attributes: popover.floating_events(),
            style: popover.style(),
            onmounted: (*popover.floating()).mount(),
            onkeydown: move |event: KeyboardEvent| {
                if !event.default_action_enabled() {
                    return;
                }
                if event.key() == Key::Named(NamedKey::Escape) {
                    event.prevent_default();
                    event.stop_propagation();
                    focus_by_id(&escape_trigger_id);
                    if editing.peek().is_some() {
                        editing.set(None);
                    } else {
                        escape_close.call(());
                    }
                }
            },
            sx: floating_sx,
            if loaded() {
                RolePanel { editing, on_close: close, on_layout_change: remeasure, conversation, catalog, pump, settings, error }
            } else if !error().is_empty() {
                Text { role: "alert", "{error}" }
            } else {
                Text { color: "muted", "Loading model roles…" }
            }
        }
    }));
    rsx! {
        Button {
            variant: "standard",
            id: "{trigger_id}",
            onmounted: anchor.mount(),
            attributes: popover.anchor_events(),
            onclick: move |_| open.toggle(),
            aria_expanded: open(),
            aria_controls: "{content_id}",
            "data-testid": "agent-chat-model-trigger",
            title: trigger_title,
            Text { sx: trigger_label_sx, "{trigger_label}" }
        }
    }
}
