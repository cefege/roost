//! The searchable Settings rail and compact list/detail layout.
//! `settings_navigation` owns destinations and descriptions for this shell
//! and the `/design` specimen. Each pane owns its data and mutations.

pub mod agent_accounts;
pub mod agent_connect_dialog;
pub mod agent_launcher;
pub mod agent_login_dialog;
pub mod agent_models;
pub mod agent_provider_list;
pub mod agent_provider_status;
pub mod agent_roles;
pub mod agents;
pub mod attachments;
pub mod audit;
pub mod color_schemes;
pub mod connection;
pub mod devices;
pub mod format;
pub mod machines;
pub mod mcp;
pub mod metrics;
pub mod model_chain;
pub mod model_chain_editor;
pub mod model_scope;
pub mod notifications;
pub mod terminal;
pub mod theme;
pub mod voice;
pub mod voice_languages;

mod pane;

use dioxus::html::input_data::MouseButton;
use dioxus::prelude::*;

use crate::components::layout::window_size::use_is_compact;
use crate::components::settings_navigation::{SETTINGS_GROUPS, SettingsPaneSpec, settings_pane};
use crate::components::ui::list::is_in_app_navigation_click;
use crate::components::ui::sidenav_layout::{
    SidenavGroup, SidenavGroupContent, SidenavGroupLabel, SidenavMenu, SidenavMenuItem,
};
use crate::components::ui::sidenav_links::SidenavMenuButton;
use crate::components::ui::{
    Empty, EmptyDescription, EmptyHeader, EmptyMedia, EmptyMediaVariant, EmptyTitle, Icon,
    IconButton, IconType, List, TextField,
};
use crate::router_state::use_navigate;
use crate::routes::Route;

/// The pane the desktop editor shows when the URL names none — and the pane it
/// shows when the URL names one this build does not have.
///
/// v2's fallback, and the reason it is not a redirect: the rail's navigation
/// order starts here, and a retired pane's old bookmark should land on a
/// working editor rather than bounce the reader somewhere they did not ask for.
pub const DEFAULT_SETTINGS_PANE: &str = "machines";

/// The pane the URL names, or the default. `None` only when the URL carries no
/// pane at all, which is what the compact branch reads to keep `/settings` the
/// category list rather than a detail page.
pub fn selected_pane(pane: Option<&str>) -> Option<&'static SettingsPaneSpec> {
    match pane {
        None => None,
        Some(id) => Some(settings_pane(id).unwrap_or_else(|| {
            settings_pane(DEFAULT_SETTINGS_PANE).unwrap_or(&SETTINGS_GROUPS[0].panes[0])
        })),
    }
}

/// The editor pane for a URL, falling back to the rail's first.
fn editor_pane(pane: Option<&str>) -> &'static SettingsPaneSpec {
    selected_pane(pane).unwrap_or_else(|| &SETTINGS_GROUPS[0].panes[0])
}

/// `/settings` and `/settings/:pane`.
#[component]
pub fn SettingsSurface(route: Route) -> Element {
    let Route::Settings { pane } = route else {
        return rsx! {};
    };
    let compact = use_is_compact();
    let spec = selected_pane(pane.as_deref());
    let active = editor_pane(pane.as_deref());
    rsx! {
        if compact {
            match spec {
                Some(spec) => rsx! { MobileSettingsDetail { spec: Some(*spec) } },
                None => rsx! { MobileSettingsList {} },
            }
        } else {
            SettingsEditor { active: Some(*active) }
        }
    }
}

/// The desktop workbench: the rail on the left, one pane in the editor.
#[component]
fn SettingsEditor(active: Option<SettingsPaneSpec>) -> Element {
    let navigate = use_navigate();
    let mut query = use_signal(String::new);
    let groups = search_groups(&query());
    let Some(active) = active else {
        return rsx! {};
    };
    rsx! {
        div { class: "flex h-full min-h-0 w-full bg-background",
            aside {
                class: "flex w-72 shrink-0 flex-col gap-4 overflow-y-auto border-r border-sidenav-border bg-sidenav p-4",
                "aria-label": "Settings sections",
                h1 { class: "px-2 text-lg font-semibold text-foreground", "Settings" }
                TextField {
                    value: query(),
                    on_input: move |value| query.set(value),
                    input_type: "search",
                    placeholder: "Search settings…",
                    aria_label: "Search settings",
                    test_id: "settings-search",
                }
                for (label, panes) in &groups {
                    SidenavGroup {
                        SidenavGroupLabel { "{label}" }
                        SidenavGroupContent {
                            SidenavMenu {
                                for pane in panes {
                                    {
                                        let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                        let is_active = pane.id == active.id;
                                        rsx! {
                                            SidenavMenuItem {
                                                SidenavMenuButton {
                                                    href: Some(href.clone()),
                                                    aria_current: is_active.then(|| "page".to_owned()),
                                                    "data-testid": format!("rail-{}", pane.id),
                                                    onclick: move |event: MouseEvent| {
                                                        let primary = event.trigger_button() == Some(MouseButton::Primary);
                                                        if is_in_app_navigation_click(primary, event.modifiers()) {
                                                            event.prevent_default();
                                                            navigate.call(href.clone());
                                                        }
                                                    },
                                                    Icon { icon: pane.icon }
                                                    span { title: pane.description, {pane.label} }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if groups.is_empty() {
                    p { class: "px-3 text-sm text-muted-foreground", role: "status", "No matching settings" }
                }
            }
            main { class: "flex min-w-0 flex-1 flex-col",
                header { class: "flex h-14 shrink-0 items-center gap-2 border-b border-border px-4",
                    IconButton {
                        icon: IconType::ArrowLeft,
                        label: "Back to app",
                        "data-testid": "settings-back",
                        onclick: move |_| navigate.call(Route::Home.to_path()),
                    }
                    h1 { class: "truncate text-base font-semibold text-foreground", {active.title} }
                }
                div { class: "min-h-0 flex-1 overflow-y-auto",
                    div { class: "mx-auto flex w-full max-w-3xl flex-col gap-6 p-6",
                        p { class: "text-sm text-muted-foreground", {active.description} }
                        pane::SettingsPane { id: active.id }
                    }
                }
            }
        }
    }
}

/// The compact category list, which stays the settings home at `/settings`.
#[component]
fn MobileSettingsList() -> Element {
    let navigate = use_navigate();
    let mut query = use_signal(String::new);
    let groups = search_groups(&query());
    rsx! {
        div { class: "flex h-full min-h-0 flex-col bg-background",
            header { class: "flex h-14 shrink-0 items-center gap-2 border-b border-border px-4",
                IconButton {
                    icon: IconType::ArrowLeft,
                    label: "Back to app",
                    "data-testid": "settings-back",
                    onclick: move |_| navigate.call(Route::Home.to_path()),
                }
                h1 { class: "truncate text-base font-semibold text-foreground", "Settings" }
            }
            div { class: "px-4 py-2",
                TextField {
                    value: query(),
                    on_input: move |value| query.set(value),
                    input_type: "search",
                    placeholder: "Search settings…",
                    aria_label: "Search settings",
                    test_id: "settings-search",
                }
            }
            List { class: "gap-4 overflow-y-auto p-2",
                for (label, panes) in &groups {
                    SidenavGroup {
                        SidenavGroupLabel { "{label}" }
                        SidenavGroupContent {
                            SidenavMenu {
                                for pane in panes {
                                    {
                                        let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                        rsx! {
                                            SidenavMenuItem {
                                                SidenavMenuButton {
                                                    href: Some(href.clone()),
                                                    "data-testid": format!("settings-list-{}", pane.id),
                                                    onclick: move |event: MouseEvent| {
                                                        let primary = event.trigger_button() == Some(MouseButton::Primary);
                                                        if is_in_app_navigation_click(primary, event.modifiers()) {
                                                            event.prevent_default();
                                                            navigate.call(href.clone());
                                                        }
                                                    },
                                                    Icon { icon: pane.icon }
                                                    span { class: "min-w-0 flex-1",
                                                        span { class: "block truncate", {pane.label} }
                                                        span { class: "block truncate text-xs font-normal text-muted-foreground", {pane.description} }
                                                    }
                                                    Icon { icon: IconType::ChevronRight }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if groups.is_empty() {
                    Empty {
                        EmptyHeader {
                            EmptyMedia { variant: EmptyMediaVariant::Icon,
                                Icon { icon: IconType::Search }
                            }
                            EmptyTitle { "No matching settings" }
                            EmptyDescription { "Try a section name, model, account or connection." }
                        }
                    }
                }
            }
        }
    }
}

/// The compact detail page for one pane.
#[component]
fn MobileSettingsDetail(spec: Option<SettingsPaneSpec>) -> Element {
    let navigate = use_navigate();
    let Some(spec) = spec else {
        return rsx! {};
    };
    rsx! {
        div { class: "flex h-full min-h-0 flex-col bg-background",
            header { class: "flex h-14 shrink-0 items-center gap-2 border-b border-border px-4",
                IconButton {
                    icon: IconType::ArrowLeft,
                    label: "Back to settings",
                    "data-testid": "settings-detail-back",
                    onclick: move |_| navigate.call(Route::Settings { pane: None }.to_path()),
                }
                h1 { class: "truncate text-base font-semibold text-foreground", {spec.title} }
            }
            div { class: "min-h-0 flex-1 overflow-y-auto",
                div { class: "mx-auto flex w-full max-w-3xl flex-col gap-6 p-6",
                    p { class: "text-sm text-muted-foreground", {spec.description} }
                    pane::SettingsPane { id: spec.id }
                }
            }
        }
    }
}

fn search_groups(query: &str) -> Vec<(&'static str, Vec<SettingsPaneSpec>)> {
    let query = query.trim().as_bytes();
    SETTINGS_GROUPS
        .iter()
        .filter_map(|group| {
            let panes: Vec<_> = group
                .panes
                .iter()
                .copied()
                .filter(|pane| {
                    query.is_empty()
                        || [
                            group.label,
                            pane.id,
                            pane.label,
                            pane.title,
                            pane.description,
                            pane.search_terms,
                        ]
                        .into_iter()
                        .any(|text| {
                            text.as_bytes()
                                .windows(query.len())
                                .any(|window| window.eq_ignore_ascii_case(query))
                        })
                })
                .collect();
            (!panes.is_empty()).then_some((group.label, panes))
        })
        .collect()
}

#[cfg(test)]
mod search_tests {
    use super::search_groups;

    #[test]
    fn find_model_controls_by_work_name_or_command() {
        for query in [
            "advisor",
            "Adviser",
            "planning",
            "side tasks",
            "btw",
            "fallback",
        ] {
            let matches: Vec<_> = search_groups(query)
                .into_iter()
                .flat_map(|(_, panes)| panes)
                .map(|pane| pane.id)
                .collect();
            assert_eq!(matches, ["models"], "search: {query}");
        }
    }
}
