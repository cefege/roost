//! The newer-build notice in the dock stack. `use_stale_build` compares this
//! document's `roost-build-sha` meta stamp with a `no-store` re-fetch of
//! `/index.html` each minute while visible; Reload loads the freshly stamped
//! bundle, which clears it. Later hides it for that build only.

use dioxus::prelude::*;

use crate::components::terminal::dom::{page_visible, sleep_ms};
use crate::components::ui::{Button, ButtonSize, ButtonVariant, ToastPresentation, ToastType};

/// The meta name both the running document and the served one carry.
const BUILD_SHA_META: &str = "roost-build-sha";

/// How often a visible tab re-reads the served stamp. A deploy that lands while
/// the operator is on another tab is picked up on the next tick, without a
/// second window listener beside the app root's input owners.
const RECHECK_COOLDOWN_MS: u64 = 60_000;

/// A served build this tab is not running and has not dismissed.
#[derive(Debug, Clone, PartialEq)]
pub struct StaleBuild {
    served_sha: String,
    dismissed: Signal<Option<String>>,
}

impl StaleBuild {
    /// Hide the notice for this served build only; a newer deploy re-raises it.
    pub fn dismiss(&self) {
        let mut dismissed = self.dismissed;
        dismissed.set(Some(self.served_sha.clone()));
    }
}

/// Re-reads the served stamp every minute while the tab is visible, and yields
/// the stale build while one is pending.
pub fn use_stale_build() -> Option<StaleBuild> {
    let mut served = use_signal(|| None::<String>);
    let dismissed = use_signal(|| None::<String>);

    use_future(move || async move {
        loop {
            if page_visible()
                && let Some(body) = fetch_index_html().await
                && let Some(sha) = served_sha_of(body.as_str())
            {
                served.set(Some(sha));
            }
            sleep_ms(RECHECK_COOLDOWN_MS).await;
        }
    });

    let running = running_build_sha();
    let served_sha = served();
    if !is_stale(
        running.as_deref(),
        served_sha.as_deref(),
        dismissed().as_deref(),
    ) {
        return None;
    }
    served_sha.map(|served_sha| StaleBuild {
        served_sha,
        dismissed,
    })
}

/// The newer-build card: Reload loads the freshly stamped bundle, Later hides it.
#[component]
pub fn VersionToast(stale: StaleBuild) -> Element {
    rsx! {
        div { "data-testid": "version-banner",
            ToastPresentation {
                variant: ToastType::Info,
                title: "Roost just updated",
                description: "A newer version is ready. Your sessions are safe — reload when convenient.",
                role: "status",
                aria_live: "polite",
                close_label: "Later",
                on_close: move |_event: MouseEvent| stale.dismiss(),
                actions: rsx! {
                    Button {
                        variant: ButtonVariant::Default,
                        size: ButtonSize::Sm,
                        "data-testid": "version-banner-reload",
                        onclick: move |event: MouseEvent| {
                            event.stop_propagation();
                            reload();
                        },
                        "Reload now"
                    }
                },
            }
        }
    }
}

/// Whether the served build is one this tab is not running.
///
/// Both stamps must be real before anything is claimed: an unstamped document
/// is a development build, and a development build is never stale. A dismissed
/// sha suppresses the nudge for THAT build only, so a newer deploy re-nudges.
fn is_stale(running: Option<&str>, served: Option<&str>, dismissed: Option<&str>) -> bool {
    let (Some(running), Some(served)) = (running, served) else {
        return false;
    };
    if running.is_empty() || served.is_empty() || running == "dev" || served == "dev" {
        return false;
    }
    if dismissed == Some(served) {
        return false;
    }
    running != served
}

/// The stamp on the document this tab is running.
fn running_build_sha() -> Option<String> {
    meta_content(BUILD_SHA_META)
}

/// The stamp on a freshly served `index.html`.
///
/// Read out of the markup rather than through a parser: the coordinator serves
/// this file itself, and a second parser for one meta tag is a second place for
/// a served shell to be interpreted differently from the one that built it.
fn served_sha_of(html: &str) -> Option<String> {
    let marker = format!("name=\"{BUILD_SHA_META}\"");
    let at = html.find(&marker)?;
    let open = html[at..].find('>')? + at + 1;
    let close = html[open..].find('<')? + open;
    let value = html[open..close].trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// This document's own value for a meta name.
#[cfg(target_arch = "wasm32")]
fn meta_content(name: &str) -> Option<String> {
    let document = web_sys::window()?.document()?;
    let selector = format!("meta[name=\"{name}\"]");
    let element = document.query_selector(&selector).ok()??;
    let value = element.get_attribute("content")?;
    (!value.is_empty()).then_some(value)
}

/// No document, no stamp, and a stamp is the only thing this banner compares.
#[cfg(not(target_arch = "wasm32"))]
fn meta_content(_name: &str) -> Option<String> {
    None
}

/// Fetch the served shell. The coordinator serves it `no-store`, so this yields
/// the dist on disk right now rather than a cached copy of what already runs.
#[cfg(target_arch = "wasm32")]
async fn fetch_index_html() -> Option<String> {
    use wasm_bindgen::JsCast;

    let window = web_sys::window()?;
    let response = wasm_bindgen_futures::JsFuture::from(window.fetch_with_str("/index.html"))
        .await
        .ok()?;
    let response: web_sys::Response = response.dyn_into().ok()?;
    if !response.ok() {
        return None;
    }
    let text = wasm_bindgen_futures::JsFuture::from(response.text().ok()?)
        .await
        .ok()?;
    text.as_string()
}

/// Nothing to fetch outside a browser.
#[cfg(not(target_arch = "wasm32"))]
async fn fetch_index_html() -> Option<String> {
    None
}

/// Reload onto the served build.
fn reload() {
    #[cfg(target_arch = "wasm32")]
    if let Some(window) = web_sys::window() {
        let _ = window.location().reload();
    }
}
