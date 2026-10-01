//! The address bar, read and written. The only place `roost-web` touches the
//! document's location, and the only place a navigation happens.
//!
//! `routes::Route` is the grammar and it is pure: it parses a path and writes one
//! back with no document involved. This module supplies the one thing the
//! grammar cannot supply for itself — the path the browser is actually showing —
//! and performs the one thing a component must not do for itself, which is move
//! the address. A component that builds a link uses `Route::to_path`; a
//! component that navigates calls `navigate`.
//!
//! Behind `#[cfg(target_arch = "wasm32")]`: which route a component renders is
//! `routes::parse` on a string, and that is native and tested. Reading
//! `location` is a document read, and a native build has no document to read.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsValue;

/// The path and query the browser is currently showing, without the origin.
///
/// `pathname` plus `search` and no hash: a route is chosen by path, and a
/// fragment credential is scrubbed by `platform::fragment_credential` before the
/// router mounts, so the hash is not a route input. The leading `/` is always
/// present, and an address bar with no path reads as `/`.
#[cfg(target_arch = "wasm32")]
pub fn current_location() -> String {
    let Some(window) = web_sys::window() else {
        return "/".to_string();
    };
    let location = window.location();
    let path = location.pathname().unwrap_or_else(|_| "/".to_string());
    match location.search() {
        Ok(search) if !search.is_empty() => format!("{path}{search}"),
        _ => path,
    }
}

/// Move the address bar to a path, without reloading the document.
///
/// `pushState` rather than assigning `href`: a full load would tear down the
/// client core and every replica with it, which is the opposite of what a
/// navigation inside a single-page app is for. A caller that wants a reload has
/// a bug, not a use case.
#[cfg(target_arch = "wasm32")]
pub fn navigate(href: &str) {
    let Some(window) = web_sys::window() else {
        return;
    };
    // `history()` is fallible in this web-sys, and a browser that refuses it
    // and a browser that refuses the push are the same event to a reader: the
    // address bar did not move. One warning covers both, and the shell still
    // repaints from the signal — which is why that one moves both.
    let refused = match window.history() {
        Err(_) => true,
        Ok(history) => history
            .push_state_with_url(&JsValue::NULL, "", Some(href))
            .is_err(),
    };
    if refused {
        tracing::warn!(target: "router", href, "the browser refused the navigation");
    }
}

/// Replace the document with `href`, reloading it (v2 `location.replace`). The
/// one navigation that is meant to tear the client down: a redeemed pairing
/// re-boots the page as a paired key.
#[cfg(target_arch = "wasm32")]
pub fn replace_location(href: &str) {
    let replaced = web_sys::window().is_some_and(|window| window.location().replace(href).is_ok());
    if !replaced {
        tracing::warn!(target: "router", href, "the browser refused the reload");
    }
}

/// Reload the current document, staying on the same address.
///
/// The one reload that is not a navigation: the address does not change, so the
/// re-mounted document re-reads the same tab-scoped records and resumes whatever
/// it was following. v2 reaches for it in exactly one place — an approver whose
/// code the client can no longer track (`PairApprovalProvider.tsx:360`) — and
/// the address-preserving part is the whole point: replacing to `/` would throw
/// the reader out of the page they were on to fix.
#[cfg(target_arch = "wasm32")]
pub fn reload_document() {
    if web_sys::window().is_none_or(|window| window.location().reload().is_err()) {
        tracing::warn!(target: "router", "the browser refused the document reload");
    }
}

/// A native build has no address bar, so every path reads as the root.
///
/// This is a stub and not a second grammar: `routes::Route` still parses
/// whatever this returns, so a native test drives the same components a
/// browser runs rather than a set that exists only natively.
#[cfg(not(target_arch = "wasm32"))]
pub fn current_location() -> String {
    "/".to_string()
}

/// A native build has no history to move, so a navigation only repaints.
#[cfg(not(target_arch = "wasm32"))]
pub fn navigate(_href: &str) {}

/// A native build has no document to reload.
#[cfg(not(target_arch = "wasm32"))]
pub fn replace_location(_href: &str) {}

/// A native build has no document to reload, and paints nothing to reload.
#[cfg(not(target_arch = "wasm32"))]
pub fn reload_document() {}
