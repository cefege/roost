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

use crate::components::layout::window_size::use_is_compact;
use crate::components::settings_navigation::{SETTINGS_GROUPS, SettingsPaneSpec, settings_pane};
use crate::router_state::use_navigate;
use crate::routes::Route;
use dioxus::prelude::*;
use libero::components::{Button, Flex, NavLink, Paper, Pictogram, Text, TextField, Title};
use libero::sx::sx;
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
    let editor_sx = use_hook(|| {
        sx().flex("1")
            .min_width("0")
            .min_height("0")
            .overflow("hidden")
    });
    let rail_sx = use_hook(|| {
        sx().flex_shrink("0")
            .min_height("0")
            .overflow_y("auto")
            .with("overscroll-behavior-y", "contain")
    });
    let pane_sx = use_hook(|| {
        sx().flex("1")
            .min_width("0")
            .min_height("0")
            .overflow_y("auto")
            .with("overscroll-behavior-y", "contain")
    });
    let Some(active) = active else {
        return rsx! {};
    };
    let active_id = active.id.to_owned();
    rsx! {
        Flex { direction: "row", align: "stretch", wrap: false, sx: editor_sx, "data-testid": "settings-editor",
            Paper { bordered: true, sx: rail_sx,
                Flex { direction: "column", gap: "sm",
                    Title { size: "lg", "Settings" }
                    TextField {
                        value: query(),
                        r#type: "search",
                        placeholder: "Search settings…",
                        label: "Search settings",
                        "data-testid": "settings-search",
                        oninput: move |value| query.set(value),
                    }
                    for (label, panes) in &groups {
                        Flex { direction: "column", gap: "xs",
                            Text { size: "xs", color: "muted", "{label}" }
                            for pane in panes {
                                {
                                    let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                    rsx! {
                                        NavLink {
                                            to: href.clone(),
                                            onclick: move |event: MouseEvent| {
                                                event.prevent_default();
                                                navigate.call(href.clone());
                                            },
                                            active: pane.id == active_id,
                                            description: pane.description,
                                            leading: Some(rsx! { Pictogram { icon: pane.icon, width: "18", height: "18" } }),
                                            "data-testid": format!("rail-{}", pane.id),
                                            "{pane.label}"
                                        }
                                    }
                                }
                            }
                        }
                    }
                    if groups.is_empty() {
                        Text { size: "sm", color: "muted", role: "status", "No matching settings" }
                    }
                }
            }
            Flex { direction: "column", gap: "md", sx: pane_sx,
                Flex { direction: "row", align: "center", gap: "sm",
                    Button {
                        variant: "standard",
                        icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::arrow_left::outlined, width: "18", height: "18" } }),
                        "aria-label": "Back to app",
                        "data-testid": "settings-back",
                        onclick: move |_| navigate.call(Route::Home.to_path()),
                        "Back"
                    }
                    Title { size: "lg", {active.title} }
                }
                Text { size: "sm", color: "muted", {active.description} }
                Paper { bordered: true,
                    pane::SettingsPane { id: active.id }
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
        Flex { direction: "column", gap: "sm",
            Flex { direction: "row", align: "center", gap: "sm",
                Button {
                    variant: "standard",
                    icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::arrow_left::outlined, width: "18", height: "18" } }),
                    "aria-label": "Back to app",
                    "data-testid": "settings-back",
                    onclick: move |_| navigate.call(Route::Home.to_path()),
                    "Back"
                }
                Title { size: "lg", "Settings" }
            }
            TextField {
                value: query(),
                r#type: "search",
                placeholder: "Search settings…",
                label: "Search settings",
                "data-testid": "settings-search",
                oninput: move |value| query.set(value),
            }
            Flex { direction: "column", gap: "md",
                for (label, panes) in &groups {
                    Flex { direction: "column", gap: "xs",
                        Text { size: "xs", color: "muted", "{label}" }
                        for pane in panes {
                            {
                                let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                rsx! {
                                    NavLink {
                                        to: href.clone(),
                                        onclick: move |event: MouseEvent| {
                                            event.prevent_default();
                                            navigate.call(href.clone());
                                        },
                                        leading: Some(rsx! { Pictogram { icon: pane.icon, width: "18", height: "18" } }),
                                        trailing: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::chevron_right::outlined, width: "16", height: "16" } }),
                                        description: pane.description,
                                        "data-testid": format!("settings-list-{}", pane.id),
                                        "{pane.label}"
                                    }
                                }
                            }
                        }
                    }
                }
                if groups.is_empty() {
                    Paper { bordered: true,
                        Flex { direction: "column", gap: "xs",
                            Text { size: "md", "No matching settings" }
                            Text { size: "sm", color: "muted", "Try a section name, model, account or connection." }
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
        Flex { direction: "column", gap: "md",
            Flex { direction: "row", align: "center", gap: "sm",
                Button {
                    variant: "standard",
                    icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::arrow_left::outlined, width: "18", height: "18" } }),
                    "aria-label": "Back to settings",
                    "data-testid": "settings-detail-back",
                    onclick: move |_| navigate.call(Route::Settings { pane: None }.to_path()),
                    "Back"
                }
                Title { size: "lg", {spec.title} }
            }
            Text { size: "sm", color: "muted", {spec.description} }
            Paper { bordered: true,
                pane::SettingsPane { id: spec.id }
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
