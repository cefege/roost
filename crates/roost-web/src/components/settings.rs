//! The Settings shell: the rail of groups, the pane top bar, and the compact
//! list/detail pair. Ported from `apps/web/src/components/Settings/SettingsRoot.tsx`,
//! `MobileSettingsList.tsx` and `MobileSettingsDetail.tsx`; `app::surface_for`
//! mounts [`SettingsSurface`] for `Route::Settings`.
//!
//! The rail's contents are `settings_navigation::SETTINGS_GROUPS`, the same
//! definition the `/design` specimen reads, so navigation order has one owner.
//! One module per pane below, each keeping its own data and mutations, exactly
//! as v2's `SettingsPane.tsx` switch did.

pub mod agent_accounts;
pub mod agent_connect_dialog;
pub mod agent_launcher;
pub mod agent_login_dialog;
pub mod agent_models;
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
pub mod notifications;
pub mod terminal;
pub mod theme;
pub mod voice;
pub mod voice_languages;

mod pane;

use dioxus::prelude::*;

use crate::components::layout::window_size::use_is_compact;
use crate::components::md::{Icon, IconButton, List, ListRow};
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
    let Some(active) = active else {
        return rsx! {};
    };
    rsx! {
        div { class: "settings-shell",
            aside { class: "settings-rail", "aria-label": "Settings sections",
                h1 { class: "settings-rail__title", "Settings" }
                for group in SETTINGS_GROUPS {
                    section { class: "settings-rail__group",
                        h2 { class: "settings-rail__group-label", {group.label} }
                        for pane in group.panes {
                            ListRow {
                                class: "settings-rail__item",
                                href: Route::Settings { pane: Some(pane.id.to_string()) }.to_path(),
                                on_navigate: navigate,
                                selected: pane.id == active.id,
                                aria_current: (pane.id == active.id).then(|| "page".to_string()),
                                test_id: Some(format!("rail-{}", pane.id)),
                                leading: rsx! { Icon { name: pane.icon.to_string() } },
                                headline: rsx! { {pane.label} },
                            }
                        }
                    }
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
            List { class: "settings-mobile__list",
                for group in SETTINGS_GROUPS {
                    div { class: "settings-mobile__group",
                        div { class: "settings-mobile__group-label", {group.label} }
                        for pane in group.panes {
                            {
                                let href = Route::Settings { pane: Some(pane.id.to_string()) }.to_path();
                                rsx! {
                                    ListRow {
                                        class: "settings-mobile__row",
                                        test_id: Some(format!("settings-list-{}", pane.id)),
                                        onclick: move |_| navigate.call(href.clone()),
                                        leading: rsx! { Icon { name: pane.icon.to_string(), class: "settings-mobile__icon" } },
                                        headline: rsx! { span { class: "settings-mobile__label", {pane.label} } },
                                        trailing: rsx! { Icon { name: "chevron_right", class: "settings-mobile__chev" } },
                                    }
                                }
                            }
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
                    pane::SettingsPane { id: spec.id }
                }
            }
        }
    }
}
