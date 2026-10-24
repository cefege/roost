//! Model hub views, filtering and keyboard interaction.
//! Owned by `model_hub.rs`'s modal; child modules supply the view pieces and actions.
//! This module keeps the content state separate from conversation-scoped modal lifetime.
use super::model_hub_actions;
use dioxus::prelude::*;
use libero::components::{Box, Flex, Skeleton, Text, TextField};
use libero::sx::sx;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelsCatalog, ProviderEntry,
};

use super::hub_browse::{BrowseRow, browse_rows};
use super::hub_catalog::{kinds_present, provider_configured, roles_progress};
use super::hub_keys::{HubFocus, HubKeyContext, HubKeyEffect, hub_key_effect};
use super::hub_list::{HubSidebar, KindTabs, ModelDetail, ModelRows};
use super::hub_logic::{HubView, hub_sections};
use super::hub_text::{footer_hints, status_text};
use super::locked_provider::LockedProvider;
use super::model_hub_actions::{
    HUB_ROOT_ID, HubKeySignals, StripCommit, activate_row, activate_strip_chip, apply_key_effect,
    conversation_folder, opened_strip, refocus_frame, rescope_strip,
};
use super::model_hub_keys::to_hub_key;
use super::model_role_strip::{ModelRoleStrip, OpenStrip, STRIP_FOOTER, run_strip_key};
use super::roles::HubRolesView;
use super::strip_chips::{strip_key, strip_view_chips};
use crate::components::settings::agent_roles::load_settings;
use crate::pump::Pump;

#[component]
pub(super) fn ModelHubContent(
    open: Signal<bool>,
    request_generation: Signal<u64>,
    conversation: ConversationSummary,
    catalog: Option<ModelsCatalog>,
    pump: Pump,
    mut connect_provider: Signal<Option<ProviderEntry>>,
) -> Element {
    let mut open = open;
    let mut view = use_signal(|| HubView::Roles);
    let mut kind = use_signal(|| "chat".to_owned());
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| 0_usize);
    let mut focus_pane = use_signal(|| HubFocus::List);
    let settings = use_signal(AgentSettingsView::default);
    let settings_loaded = use_signal(|| false);
    let mut error = use_signal(String::new);
    let mut strip = use_signal(|| None::<OpenStrip>);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() && request_generation() > 0 {
            view.set(HubView::Roles);
            query.set(String::new());
            selected.set(0);
            error.set(String::new());
            strip.set(None);
            load_settings(load_pump.clone(), settings, error, settings_loaded);
            refocus_frame();
        }
    });
    let loading_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .padding("lg")
    });
    let body_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .flex_grow("1")
            .min_height("0")
            .gap("sm")
            .padding("lg")
    });
    let grid_sx = use_hook(|| {
        sx().display("grid")
            .flex("1")
            .with(
                "grid-template-columns",
                "minmax(10rem, 14rem) minmax(0, 1fr)",
            )
            .with("grid-template-rows", "minmax(0, 1fr)")
            .gap("md")
            .min_height("0")
            .media(
                "(max-width: 40rem)",
                sx().with("grid-template-columns", "minmax(0, 1fr)")
                    .with("grid-template-rows", "auto minmax(0, 1fr)"),
            )
    });
    let sidebar_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("xs")
            .min_height("0")
            .overflow_y("auto")
            .with("overscroll-behavior", "contain")
            .media("(max-width: 40rem)", sx().max_height("9rem"))
    });
    let main_sx = use_hook(|| {
        sx().display("flex")
            .flex_direction("column")
            .gap("sm")
            .min_height("0")
            .min_width("0")
            .overflow_y("auto")
            .with("overscroll-behavior", "contain")
    });
    let section_sx = use_hook(|| sx().flex_shrink("0"));
    let footer_sx = use_hook(|| {
        sx().flex_shrink("0")
            .border_top("1px solid")
            .border_color("muted.4")
            .padding_top("sm")
    });
    if !open() {
        return rsx! {};
    }
    let Some(catalog) = catalog.filter(|_| settings_loaded()) else {
        return rsx! {
            Flex { direction: "column", sx: loading_sx, "data-testid": "agent-model-hub", "data-name": "ModelHub",
                if !error().is_empty() {
                    Text { size: "sm", color: "error", role: "alert", "{error()}" }
                } else {
                    div { role: "status", "aria-label": "Loading models",
                        for index in 0..4 { Skeleton { key: "{index}" } }
                    }
                }
            }
        };
    };

    let sections = hub_sections(&catalog);
    let current = view();
    let current_kind = kind();
    let search = query();
    let settings_now = settings();
    let rows: Vec<BrowseRow> = if !matches!(current, HubView::Roles) {
        browse_rows(&catalog, &settings_now, &current, &current_kind, &search)
    } else {
        Vec::new()
    };
    let row_count = rows.len();
    let index = selected().min(row_count.saturating_sub(1));
    let selected_row = rows.get(index).cloned();
    let (roles_assigned, roles_total) = roles_progress(&settings_now);
    let locked = matches!(&current, HubView::Provider(id) if !provider_configured(&catalog, id));
    let kinds = kinds_present(&catalog);
    let focus = focus_pane();
    let footer = footer_hints(&current, locked, focus);
    let status = status_text(&current, &catalog);
    let current_selector = conversation
        .model
        .as_ref()
        .or(conversation.effective_model.as_ref())
        .map(|model| format!("{}/{}", model.provider, model.model_id));
    let folder = conversation_folder(&conversation);

    let strip_view = strip()
        .filter(|open_strip| {
            selected_row
                .as_ref()
                .is_some_and(|row| row.selector == open_strip.selector)
        })
        .map(|open_strip| {
            let chips = strip_view_chips(
                &settings_now,
                &catalog,
                &open_strip,
                current_selector.as_deref(),
            );
            (open_strip, chips)
        });
    let key_strip = strip_view.clone();
    let click_current = current_selector.clone();
    let strip_commit = StripCommit {
        pump: pump.clone(),
        conversation_id: conversation.id.clone(),
        pinned: conversation.model.is_some(),
        running: conversation.running_role(),
        settings,
        error,
        open,
        strip,
    };
    let click_commit = strip_commit.clone();
    let footer = if strip_view.is_some() {
        STRIP_FOOTER.to_owned()
    } else {
        footer
    };
    let key_catalog = catalog.clone();
    let key_rows = rows.clone();
    let key_sections = sections.clone();
    let click_catalog = catalog.clone();
    let click_rows = rows.clone();
    let key_kinds = kinds.clone();
    let strip_catalog = catalog.clone();
    let key_folder = folder.clone();
    let click_folder = folder.clone();
    let on_key = move |event: KeyboardEvent| {
        if !event.default_action_enabled() {
            return;
        }
        let Some(key) = to_hub_key(&event) else {
            return;
        };
        if let Some((open_strip, chips)) = &key_strip {
            match strip_key(&key) {
                Some(strip_action) => {
                    event.prevent_default();
                    event.stop_propagation();
                    run_strip_key(
                        strip_action,
                        open_strip,
                        chips,
                        &key_catalog,
                        &strip_commit,
                        strip,
                    );
                    return;
                }
                None => strip.set(None),
            }
        }
        let context = HubKeyContext {
            focus: if focus_pane() == HubFocus::Scope {
                HubFocus::Scope
            } else {
                HubFocus::List
            },
            query: &query(),
            provider_view: matches!(view(), HubView::Provider(_)),
            locked_view: matches!(&view(), HubView::Provider(id) if !provider_configured(&key_catalog, id)),
        };
        let effect = hub_key_effect(&context, &key);
        if effect == HubKeyEffect::Ignore {
            return;
        }
        event.prevent_default();
        event.stop_propagation();
        let mut signals = HubKeySignals {
            view,
            kind,
            query,
            selected,
            focus_pane,
            open,
            connect_provider,
            strip,
        };
        let settings_snapshot = settings();
        let data = model_hub_actions::HubKeyData {
            sections: &key_sections,
            kinds: &key_kinds,
            catalog: &key_catalog,
            rows: &key_rows,
            settings: &settings_snapshot,
            folder: &key_folder,
            index,
            row_count,
        };
        apply_key_effect(effect, &mut signals, &data);
    };

    let sidebar_sections = sections.clone();
    rsx! {
        Flex { direction: "column", sx: body_sx,
            id: HUB_ROOT_ID,
            "data-testid": "agent-model-hub",
            tabindex: "-1",
            onkeydown: on_key,
            Text { size: "sm", color: "muted", "Set the roles every chat starts from, or pick this chat's model." }
            Box { sx: grid_sx,
                Box { component: "nav", sx: sidebar_sx, "aria-label": "Model views",
                    HubSidebar {
                        sections: sidebar_sections,
                        current: current.clone(),
                        focused: focus == HubFocus::Scope,
                        roles_assigned,
                        roles_total,
                        on_select: move |target: HubView| {
                            view.set(target);
                            selected.set(0);
                            focus_pane.set(HubFocus::List);
                            refocus_frame();
                        }
                    }
                            }
                Box { component: "section", sx: main_sx,
                                if !error().is_empty() {
                    Text { sx: section_sx.clone(), size: "sm", color: "error", role: "alert", "{error}" }
                                }
                                if matches!(current, HubView::Roles) {
                                    HubRolesView {
                                        catalog: catalog.clone(),
                                        settings,
                                        error,
                                        pump: pump.clone(),
                                        conversation: conversation.clone(),
                                        focused: focus == HubFocus::List,
                                    }
                                } else if locked {
                                    LockedProvider { catalog: catalog.clone(), view: current.clone(), connect_provider, open }
                                } else {
                                    Text { sx: section_sx.clone(), size: "sm", color: "muted", "{status}" }
                                    KindTabs {
                                        kinds: kinds.clone(),
                                        active: current_kind.clone(),
                                        on_select: move |next: String| {
                                            kind.set(next);
                                            selected.set(0);
                                        },
                                    }
                                    Box { sx: section_sx.clone(),
                                        TextField {
                                            type: "search",
                                            value: search.clone(),
                                            placeholder: "Search models".to_owned(),
                                            aria_label: "Search models",
                                            "data-testid": "agent-model-hub-picker",
                                            autocomplete: "off",
                                            // The field owns printable keys and Backspace, so the hub's key ladder
                                            // does not also apply them to the query.
                                            onkeydown: move |event: KeyboardEvent| {
                                                if matches!(event.key(), Key::Character(_) | Key::Named(NamedKey::Backspace)) {
                                                    event.stop_propagation();
                                                }
                                            },
                                            oninput: move |text: String| {
                                                query.set(text);
                                                selected.set(0);
                                            },
                                        }
                                    }
                                    ModelRows {
                                        rows: rows.clone(),
                                        selected: index,
                                        current: current_selector.clone(),
                                        on_select: move |next: usize| {
                                            selected.set(next);
                                        },
                                        on_activate: move |next: usize| {
                                            selected.set(next);
                                            focus_pane.set(HubFocus::List);
                                            refocus_frame();
                                            if let Some(selector) = activate_row(
                                                &view(),
                                                &click_catalog,
                                                &click_rows,
                                                next,
                                                &mut open,
                                                &mut connect_provider,
                                            ) {
                                                strip.set(Some(opened_strip(&settings(), &click_folder, selector)));
                                            }
                                        },
                                    }
                                    if let Some((open_strip, chips)) = &strip_view {
                                        ModelRoleStrip {
                                            chips: chips.clone(),
                                            cursor: open_strip.cursor,
                                            scope: open_strip.scope.clone(),
                                            folder: folder.clone(),
                                            on_activate: move |chip_index: usize| {
                                                activate_strip_chip(
                                                    &click_commit,
                                                    &strip_catalog,
                                                    click_current.as_deref(),
                                                    chip_index,
                                                );
                                            },
                                            on_scope: move |next_scope| rescope_strip(strip, next_scope),
                                        }
                                    }
                                    ModelDetail { row: selected_row }
                                }
                            }
                        }
                        Text { sx: footer_sx, size: "xs", color: "muted", role: "status", "aria-live": "polite", "{footer}" }
                    }
    }
}
