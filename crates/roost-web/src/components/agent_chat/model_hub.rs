//! The models hub that `/model` and `/models` open, ported from OMP's
//! `model-hub.ts`: a sidebar of views (Roles, Recent, All models, providers)
//! beside the content in a dialog. Roles mounts the role editor; the model views
//! browse rows filtered by kind and search. Enter on a model opens its role strip to
//! assign it to a role or switch this chat's model. A provider not signed in offers
//! its sign-in. Mounted by `agent_chat::surface`.

mod hub_browse;
mod hub_catalog;
mod hub_format;
mod hub_keys;
mod hub_list;
pub mod hub_logic;
mod hub_order;
mod hub_text;
mod locked_provider;
mod model_hub_actions;
mod model_hub_keys;
mod model_role_strip;
mod roles;
mod strip_chips;
mod strip_thinking;

use hub_text::{footer_hints, status_text};
use locked_provider::LockedProvider;
use model_hub_actions::{
    HUB_ROOT_ID, HubKeySignals, StripCommit, activate_row, activate_strip_chip, apply_key_effect,
    conversation_folder, opened_strip, refocus_frame, rescope_strip,
};
use model_role_strip::{ModelRoleStrip, OpenStrip, STRIP_FOOTER, run_strip_key};
use strip_chips::{strip_key, strip_view_chips};

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::{
    AgentSettingsView, ConversationSummary, ModelsCatalog, ProviderEntry,
};

use self::hub_browse::{BrowseRow, browse_rows};
use self::hub_catalog::{kinds_present, provider_configured, roles_progress};
use self::hub_keys::{HubFocus, HubKeyContext, HubKeyEffect, hub_key_effect};
use self::hub_list::{HubSidebar, KindTabs, ModelDetail, ModelRows};
use self::hub_logic::{HubView, hub_sections};
use self::model_hub_keys::to_hub_key;
use self::roles::HubRolesView;
use super::agent_alert::ERROR_TEXT_CLASS;
use crate::components::settings::agent_roles::load_settings;
use crate::components::ui::{
    Dialog, DialogBody, DialogContent, DialogDescription, DialogHeader, DialogTitle, Skeleton,
    TextField,
};
use crate::pump::Pump;

/// The hub's dialog is wider and taller than a standard dialog so the sidebar,
/// the rows and the role editor each keep their own scroll region.
const HUB_DIALOG_CLASS: &str =
    "w-[min(calc(100vw-2rem),64rem)] h-[min(85dvh,56rem)] max-w-none sm:max-w-none";

#[component]
pub fn ModelHub(
    open: Signal<bool>,
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
    let mut error = use_signal(String::new);
    let mut strip = use_signal(|| None::<OpenStrip>);
    let load_pump = pump.clone();
    use_effect(move || {
        if open() {
            view.set(HubView::Roles);
            query.set(String::new());
            selected.set(0);
            error.set(String::new());
            strip.set(None);
            load_settings(load_pump.clone(), settings, error);
            refocus_frame();
        }
    });
    if !open() {
        return rsx! {};
    }
    let close = move |next: bool| {
        if !next {
            open.set(false);
        }
    };
    let Some(catalog) = catalog else {
        return rsx! {
            Dialog { open: true, on_open_change: close,
                DialogContent { class: HUB_DIALOG_CLASS, test_id: "agent-model-hub",
                    DialogHeader {
                        DialogTitle { "Models" }
                    }
                    DialogBody { class: "min-h-0 flex-1 gap-0 p-5",
                        div { class: "flex flex-col gap-2", role: "status", "aria-label": "Loading models",
                            for index in 0..4 { Skeleton { key: "{index}", class: "h-8" } }
                        }
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
        Dialog { open: true, on_open_change: close,
            DialogContent { class: HUB_DIALOG_CLASS, test_id: "agent-model-hub",
                DialogHeader {
                    DialogTitle { "Models" }
                    DialogDescription { "Set the roles every chat starts from, or pick this chat's model." }
                }
                DialogBody { class: "min-h-0 flex-1 gap-0 overflow-hidden p-0",
                    div {
                        id: HUB_ROOT_ID,
                        class: "flex min-h-0 flex-1 flex-col gap-2 p-5 outline-none",
                        tabindex: "-1",
                        onkeydown: on_key,
                        div { class: "grid min-h-0 flex-1 grid-cols-[minmax(10rem,14rem)_minmax(0,1fr)] gap-4 max-[40rem]:grid-cols-[minmax(0,1fr)] max-[40rem]:grid-rows-[auto_minmax(0,1fr)]",
                            nav { class: "min-h-0 overflow-y-auto overscroll-contain max-[40rem]:max-h-36", "aria-label": "Model views",
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
                                    },
                                }
                            }
                            section { class: "flex min-h-0 min-w-0 flex-col gap-2 overflow-y-auto overscroll-contain",
                                if !error().is_empty() {
                                    div { class: "shrink-0",
                                        p { class: ERROR_TEXT_CLASS, role: "alert", "{error}" }
                                    }
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
                                    p { class: "shrink-0 text-sm text-muted-foreground", "{status}" }
                                    KindTabs {
                                        kinds: kinds.clone(),
                                        active: current_kind.clone(),
                                        on_select: move |next: String| {
                                            kind.set(next);
                                            selected.set(0);
                                        },
                                    }
                                    div { class: "shrink-0",
                                        TextField {
                                            class: "w-full",
                                            input_type: "search".to_owned(),
                                            value: search.clone(),
                                            placeholder: "Search models".to_owned(),
                                            aria_label: "Search models".to_owned(),
                                            test_id: "agent-model-hub-picker".to_owned(),
                                            autocomplete: "off".to_owned(),
                                            // The field owns printable keys and Backspace, so the hub's key ladder
                                            // does not also apply them to the query.
                                            onkeydown: move |event: KeyboardEvent| {
                                                if matches!(event.key(), Key::Character(_) | Key::Backspace) {
                                                    event.stop_propagation();
                                                }
                                            },
                                            on_input: move |text: String| {
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
                                            refocus_frame();
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
                        footer { class: "shrink-0 border-t border-border pt-2 text-xs text-muted-foreground", "aria-live": "polite", "{footer}" }
                    }
                }
            }
        }
    }
}
