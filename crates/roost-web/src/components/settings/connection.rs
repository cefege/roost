//! Settings → Network → Connection: which coordinator this browser talks to.
//!
//! Ports `apps/web/src/components/Settings/ConnectionPane.tsx`. The pane is the
//! UI over two `localStorage` keys — `roost.coordinatorUrl` for the active
//! origin and `roost.coords` for the saved list — read at load by
//! `platform::connect`'s base-URL resolution. Switching writes the active key
//! and reloads, because the Connect transport is built once per document.
//!
//! The saved list is component state seeded from storage, exactly as v2 held it
//! in a signal, so a second copy of the list has nowhere to drift from.
use dioxus::prelude::*;
use libero::components::{Button, Flex, Paper, Pictogram, Text, TextField};
use libero::sx::sx;
use roost_client_core::KeyValueStore;

use std::rc::Rc;

use crate::platform::location::reload_document;
use crate::platform::storage::LocalStorageKeyValueStore;

/// The key the transport reads its base URL from. The same key v2's
/// `connect.ts::coordBase` reads; a second spelling would point the transport
/// somewhere the pane did not write.
pub const ACTIVE_COORDINATOR_KEY: &str = "roost.coordinatorUrl";

/// The key the saved list is written under.
pub const SAVED_COORDINATORS_KEY: &str = "roost.coords";

/// One saved coordinator.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SavedCoordinator {
    /// What the reader called it.
    pub name: String,
    /// Its full origin, with no trailing slash.
    pub url: String,
}

/// Normalize a typed address, or `None` when it is not an `http(s)` origin.
///
/// A rejected address is rejected rather than stored: pointing the transport at
/// something that is not an origin turns every later call into a silent failure
/// with no page to fix it from.
pub fn normalise_coordinator_url(raw: &str) -> Option<String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return None;
    }
    let rest = trimmed.split_once("://")?.1;
    if rest.is_empty() || rest.contains(' ') {
        return None;
    }
    Some(trimmed.to_owned())
}

/// The saved list, or empty when storage holds something unreadable.
pub fn load_saved(storage: &dyn KeyValueStore) -> Vec<SavedCoordinator> {
    let Some(raw) = storage.get(SAVED_COORDINATORS_KEY) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

fn persist(storage: &dyn KeyValueStore, saved: &[SavedCoordinator]) {
    let encoded = serde_json::to_string(saved).unwrap_or_else(|_| "[]".to_owned());
    storage.set(SAVED_COORDINATORS_KEY, &encoded);
}

fn active_url(storage: &dyn KeyValueStore) -> String {
    storage.get(ACTIVE_COORDINATOR_KEY).unwrap_or_default()
}

/// The pane.
#[component]
pub fn ConnectionPane() -> Element {
    let storage = use_hook(|| Rc::new(LocalStorageKeyValueStore::new()));
    let saved = use_signal(|| load_saved(storage.as_ref()));
    let mut name = use_signal(String::new);
    let mut url = use_signal(String::new);
    let error = use_signal(String::new);
    let active = active_url(storage.as_ref());
    let rows = saved().clone();
    let selected_row_sx =
        use_hook(|| sx().selector("&[data-selected=\"true\"]", sx().background("primary.1")));
    let detail_sx = use_hook(|| sx().min_width("0").flex("1 1 auto"));
    rsx! {
        Flex {
            direction: "column",
            gap: "md",
            "data-testid": "settings-connection-pane",
            Paper { bordered: true,
                Flex { direction: "column", gap: "sm",
                    Flex { direction: "column", gap: "xs",
                        Text { size: "lg", "Connection" }
                        Text { size: "sm", color: "muted",
                            "Which coordinator this browser talks to. The default is the server that opened this page. Add a direct address such as localhost or another coordinator's origin."
                        }
                    }
                    Text { size: "sm", color: "muted",
                        "Cloudflare Access addresses must be opened directly in the browser, not configured here."
                    }
                    Paper { bordered: true,
                        Flex { direction: "row", align: "center", justify: "space-between", gap: "sm",
                            sx: selected_row_sx.clone(),
                            "data-selected": active.is_empty().then_some("true"),
                            Pictogram { icon: pictogram_icons_lucide::house::outlined, width: "16", height: "16" }
                            Flex { direction: "column", gap: "xs", sx: detail_sx.clone(),
                                Text { size: "sm", "This server (default)" }
                                Text { size: "xs", color: "muted",
                                    if active.is_empty() { "Connected" } else { "Same origin that served this page" }
                                }
                            }
                            if active.is_empty() {
                                Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "16", height: "16" }
                            } else {
                                Button {
                                    variant: "standard",
                                    onclick: {
                                        let storage = storage.clone();
                                        move |_| switch_to(&storage, "")
                                    },
                                    "Use"
                                }
                            }
                        }
                    }
                    for coordinator in rows {
                        Paper { bordered: true,
                            Flex { direction: "row", align: "center", justify: "space-between", gap: "sm",
                                sx: selected_row_sx.clone(),
                                "data-selected": (active == coordinator.url).then_some("true"),
                                "data-testid": format!("coord-row-{}", coordinator.url),
                                Pictogram { icon: pictogram_icons_lucide::server::outlined, width: "16", height: "16" }
                                Flex { direction: "column", gap: "xs", sx: detail_sx.clone(),
                                    Text { size: "sm", "{coordinator.name}" }
                                    Text { size: "xs", color: "muted",
                                        if active == coordinator.url {
                                            "{coordinator.url} · Connected"
                                        } else {
                                            "{coordinator.url}"
                                        }
                                    }
                                }
                                if active != coordinator.url {
                                    Button {
                                        variant: "standard",
                                        onclick: {
                                            let storage = storage.clone();
                                            let url = coordinator.url.clone();
                                            move |_| switch_to(&storage, &url)
                                        },
                                        "Use"
                                    }
                                } else {
                                    Pictogram { icon: pictogram_icons_lucide::check::outlined, width: "16", height: "16" }
                                }
                                Button {
                                    variant: "standard",
                                    icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::trash_2::outlined } }),
                                    "aria-label": "Remove",
                                    onclick: {
                                        let storage = storage.clone();
                                        let url = coordinator.url.clone();
                                        move |_| remove_coordinator(&storage, saved, &url)
                                    },
                                }
                            }
                        }
                    }
                    if saved().is_empty() {
                        Flex { direction: "column", align: "center", gap: "xs",
                            Pictogram { icon: pictogram_icons_lucide::network::outlined, width: "20", height: "20" }
                            Text { size: "sm", "No saved coordinators" }
                            Text { size: "xs", color: "muted",
                                "Add one below to reach a coord that isn't the server that served this page."
                            }
                        }
                    }
                }
            }
            Paper { bordered: true,
                Flex { direction: "column", gap: "sm",
                    Flex { direction: "column", gap: "xs",
                        Text { size: "lg", "Add a coordinator" }
                        Text { size: "sm", color: "muted",
                            "Give it a name and its full address. The address must be reachable from this device."
                        }
                    }
                    TextField {
                        label: "Name (optional)",
                        placeholder: "Home fleet",
                        value: name(),
                        oninput: move |value| name.set(value),
                    }
                    TextField {
                        label: "Address",
                        placeholder: "https://your-coordinator.ts.net:4102",
                        value: url(),
                        oninput: move |value| url.set(value),
                    }
                    if !error().is_empty() {
                        Text { size: "sm", color: "error", role: "alert", "{error()}" }
                    }
                    Flex { direction: "row",
                        Button {
                            variant: "filled",
                            icon: Some(rsx! { Pictogram { icon: pictogram_icons_lucide::plus::outlined } }),
                            onclick: {
                                let storage = storage.clone();
                                move |_| add_coordinator(&storage, saved, name, url, error)
                            },
                            "Add"
                        }
                    }
                }
            }
        }
    }
}

/// Add one coordinator to the saved list, or say why the address was refused.
fn add_coordinator(
    storage: &LocalStorageKeyValueStore,
    mut saved: Signal<Vec<SavedCoordinator>>,
    mut name: Signal<String>,
    mut url: Signal<String>,
    mut error: Signal<String>,
) {
    let Some(normalized) = normalise_coordinator_url(&url()) else {
        error.set("Enter a full address like https://your-coordinator.ts.net:4102".to_owned());
        return;
    };
    let label = match name().trim() {
        "" => normalized.clone(),
        typed => typed.to_owned(),
    };
    let mut next: Vec<SavedCoordinator> = saved()
        .into_iter()
        .filter(|coordinator| coordinator.url != normalized)
        .collect();
    next.push(SavedCoordinator {
        name: label,
        url: normalized,
    });
    persist(storage, &next);
    saved.set(next);
    name.set(String::new());
    url.set(String::new());
    error.set(String::new());
}

/// Drop one coordinator from the saved list. The active key is untouched: a
/// list entry is a convenience, and removing the one this browser is using
/// should not silently move it back to the default.
fn remove_coordinator(
    storage: &LocalStorageKeyValueStore,
    mut saved: Signal<Vec<SavedCoordinator>>,
    url: &str,
) {
    let next: Vec<SavedCoordinator> = saved()
        .into_iter()
        .filter(|coordinator| coordinator.url != url)
        .collect();
    persist(storage, &next);
    saved.set(next);
}

/// Point this browser at `url`, then reload so the transport is built again.
fn switch_to(storage: &LocalStorageKeyValueStore, url: &str) {
    if url.is_empty() {
        storage.remove(ACTIVE_COORDINATOR_KEY);
    } else {
        storage.set(ACTIVE_COORDINATOR_KEY, url);
    }
    tracing::info!(target: "settings", url, "coordinator switched; reloading");
    reload_document();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_address_is_an_http_origin_without_a_trailing_slash() {
        assert_eq!(
            normalise_coordinator_url("  https://mac.ts.net:4102/  "),
            Some("https://mac.ts.net:4102".to_owned())
        );
    }

    #[test]
    fn anything_that_is_not_an_origin_is_refused() {
        assert_eq!(normalise_coordinator_url("mac.ts.net"), None);
        assert_eq!(normalise_coordinator_url("javascript:alert(1)"), None);
        assert_eq!(normalise_coordinator_url("https://"), None);
    }
}
