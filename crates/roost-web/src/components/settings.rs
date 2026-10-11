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

use dioxus::prelude::*;

use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{EmptyState, Icon, IconButton, List, ListRow, TextField};
use crate::components::settings_navigation::{SETTINGS_GROUPS, SettingsPaneSpec, settings_pane};
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
        div { class: "settings-shell",
            aside { class: "settings-rail", "aria-label": "Settings sections",
                h1 { class: "settings-rail__title", "Settings" }
                div { class: "settings-search",
                    TextField {
                        value: query(),
                        on_input: move |value| query.set(value),
                        input_type: "search",
                        placeholder: "Search settings…",
                        aria_label: "Search settings",
                        test_id: "settings-search",
                    }
                }
                for (label, panes) in &groups {
                    section { class: "settings-rail__group",
                        h2 { class: "settings-rail__group-label", "{label}" }
                        for pane in panes {
                            ListRow {
                                class: "settings-rail__item",
                                href: Route::Settings { pane: Some(pane.id.to_string()) }.to_path(),
                                on_navigate: navigate,
                                selected: pane.id == active.id,
                                aria_current: (pane.id == active.id).then(|| "page".to_string()),
                                test_id: Some(format!("rail-{}", pane.id)),
                                leading: rsx! { Icon { name: pane.icon.to_string() } },
                                headline: rsx! { span { title: pane.description, {pane.label} } },
                            }
                        }
                    }
                }
                if groups.is_empty() {
                    p { class: "settings-search__empty", role: "status", "No matching settings" }
                }
            }
            main { class: "settings-main",
                header { class: "settings-topbar",
                    IconButton {
                        class: "settings-topbar__back",
                        icon: "arrow_back",
                        label: "Back to app",
                        "data-testid": "settings-back",
                        onclick: move |_| navigate.call(Route::Home.to_path()),
                    }
                    h1 { class: "settings-topbar__title", {active.title} }
                }
                div { class: "settings-content",
                    div { class: "settings-content__inner",
                        p { class: "settings-pane-lead", {active.description} }
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
        div { class: "settings-mobile__main",
            header { class: "settings-topbar",
                IconButton {
                    class: "settings-topbar__back",
                    icon: "arrow_back",
                    label: "Back to app",
                    "data-testid": "settings-back",
                    onclick: move |_| navigate.call(Route::Home.to_path()),
                }
                h1 { class: "settings-topbar__title", "Settings" }
            }
            div { class: "settings-search settings-search--mobile",
                TextField {
                    value: query(),
                    on_input: move |value| query.set(value),
                    input_type: "search",
                    placeholder: "Search settings…",
                    aria_label: "Search settings",
                    test_id: "settings-search",
                }
            }
            List { class: "settings-mobile__list",
                for (label, panes) in &groups {
                    div { class: "settings-mobile__group",
                        h2 { class: "settings-mobile__group-label", "{label}" }
                        for pane in panes {
                            {
                                let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                rsx! {
                                    ListRow {
                                        class: "settings-mobile__row",
                                        test_id: Some(format!("settings-list-{}", pane.id)),
                                        onclick: move |_| navigate.call(href.clone()),
                                        leading: rsx! { Icon { name: pane.icon.to_string(), class: "settings-mobile__icon" } },
                                        headline: rsx! { span { class: "settings-mobile__label", {pane.label} } },
                                        support: rsx! { span { {pane.description} } },
                                        trailing: rsx! { Icon { name: "chevron_right", class: "settings-mobile__chev" } },
                                    }
                                }
                            }
                        }
                    }
                }
                if groups.is_empty() {
                    EmptyState {
                        icon: "search",
                        title: "No matching settings",
                        supporting: "Try a section name, model, account or connection.",
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
        div { class: "settings-mobile__main",
            header { class: "settings-topbar",
                IconButton {
                    class: "settings-topbar__back",
                    icon: "arrow_back",
                    label: "Back to settings",
                    "data-testid": "settings-detail-back",
                    onclick: move |_| navigate.call(Route::Settings { pane: None }.to_path()),
                }
                h1 { class: "settings-topbar__title", {spec.title} }
            }
            div { class: "settings-content",
                div { class: "settings-content__inner",
                    p { class: "settings-pane-lead", {spec.description} }
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
