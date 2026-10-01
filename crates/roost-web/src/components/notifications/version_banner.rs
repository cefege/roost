//! The "a newer build is on disk, reload to get it" nudge, in the lower-left
//! corner under the connection banner. The running build's own stamp is read
//! from the document the browser already parsed and the served one from a
//! `no-store` re-fetch of `/index.html`; the banner fires only when they
//! differ, so Reload — a fresh `index.html`, and therefore a newly stamped
//! bundle — always clears it.
//! Ports `apps/web/src/components/notifications/VersionBanner.tsx`. v2 compared
//! against a compile-time `VITE_BUILD_SHA`; this build has no such constant,
//! and the document's own meta tag is the same fact read one hop later.

use dioxus::prelude::*;

use crate::components::md::{
    Button, ButtonSize, ButtonVariant, Icon, IconSize, Surface, SurfaceRadius,
};
use crate::components::terminal::dom::{page_visible, sleep_ms};

/// The meta name both the running document and the served one carry.
const BUILD_SHA_META: &str = "roost-build-sha";

/// How often a visible tab re-reads the served stamp. A deploy that lands while
/// the operator is on another tab is picked up on the next tick, which is the
/// answer v2's window-focus listener gave without installing a second window
/// listener beside the app root's input owners.
const RECHECK_COOLDOWN_MS: u64 = 60_000;

/// The nudge, or nothing while this build is current.
#[component]
pub fn VersionBanner() -> Element {
    let mut served = use_signal(|| None::<String>);
    let mut dismissed_sha = use_signal(|| None::<String>);

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
        dismissed_sha().as_deref(),
    ) {
        return rsx! {};
    }
    rsx! {
        div {
            "data-testid": "version-banner",
            style: "position: fixed; bottom: var(--md-space-4); left: var(--md-space-4); z-index: 49;",
            Surface {
                level: 2,
                elevation: 3,
                radius: SurfaceRadius::Md,
                style: "display: flex; align-items: flex-start; gap: var(--md-space-3); padding: var(--md-space-3) var(--md-space-4); max-width: min(42ch, calc(100vw - var(--md-space-8))); border: var(--workbench-border-width) solid var(--md-sys-color-primary); color: var(--md-sys-color-on-surface);".to_owned(),
                Icon {
                    name: "arrow_upward",
                    size: IconSize::Sm,
                    style: Some("flex-shrink: 0; color: var(--md-sys-color-primary);".to_owned()),
                }
                div {
                    style: "display: flex; flex-direction: column; gap: var(--md-space-2);",
                    span { class: "md-title-s", "Roost just updated" }
                    span {
                        class: "md-body-s",
                        style: "color: var(--md-sys-color-on-surface-variant);",
                        "A newer version is ready. Your sessions are safe — reload when convenient."
                    }
                    div {
                        style: "display: flex; gap: var(--md-space-2); margin-top: var(--md-space-1);",
                        Button {
                            variant: ButtonVariant::Default,
                            size: ButtonSize::Sm,
                            "data-testid": "version-banner-reload",
                            onclick: move |_| reload(),
                            "Reload now"
                        }
                        Button {
                            variant: ButtonVariant::Ghost,
                            size: ButtonSize::Sm,
                            "data-testid": "version-banner-later",
                            onclick: move |_| dismissed_sha.set(served_sha.clone()),
                            "Later"
                        }
                    }
                }
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
