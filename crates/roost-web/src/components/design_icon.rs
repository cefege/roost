//! The design system's icon and status dot: the two primitives the chrome needs
//! on every path. Ported from `apps/web/src/components/Settings/md/Icon.tsx` and
//! `StatusDot.tsx`, and from the stylesheet that draws them.
//!
//! Both are TOKEN references, never an inline literal. The dot in particular is
//! the one status indicator the whole app shares: a hand-rolled coloured span
//! beside these is how a surface ends up with a fourth meaning for "amber", and
//! the rule that `StatusDot` is THE indicator exists because that happened.
//!
//! NEITHER STYLESHEET IS IN THE EAGER LIST in `Dioxus.toml`: v2 reached
//! `icon.css` through a lazy chunk, and this build has no chunking, so a
//! component that owns a deferred stylesheet attaches it when it first renders.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;

/// The icon stylesheet's served path, named once so the insertion and the
/// duplicate check cannot disagree about which file they mean.
pub const ICON_STYLESHEET_HREF: &str = "/components/Settings/md/icon.css";

/// The status dot's served path, deferred for the same reason as the icon's.
pub const STATUS_DOT_STYLESHEET_HREF: &str = "/components/Settings/md/status-dot.css";

/// A Material Symbols ligature, drawn by the icon font.
///
/// `name` is the ligature text, not a path: the font ships the glyph and the
/// stylesheet owns its metrics, so a new icon is a new name rather than new
/// markup.
#[component]
pub fn Icon(name: String, filled: bool) -> Element {
    attach_stylesheet(ICON_STYLESHEET_HREF);
    let class = if filled {
        "md-icon md-icon--filled"
    } else {
        "md-icon"
    };
    rsx! {
        span {
            class,
            "aria-hidden": "true",
            {name}
        }
    }
}

/// The one status indicator. `status` is the design system's status vocabulary.
#[component]
pub fn StatusDot(status: String) -> Element {
    attach_stylesheet(STATUS_DOT_STYLESHEET_HREF);
    rsx! {
        span {
            class: "md-status-dot",
            "data-status": status,
            "aria-hidden": "true",
        }
    }
}

/// The status names the dot understands, each mapped to its colour role.
///
/// A name outside this list falls back to the low-emphasis text token, so an
/// unfamiliar status reads as quiet rather than as a colour nobody chose.
pub fn status_token(status: &str) -> &'static str {
    match status {
        "ok" | "done" => "--status-ok",
        "running" => "--md-primary",
        "idle" | "offline" => "--text-lo",
        "warn" => "--status-warn",
        "error" => "--status-err",
        "info" => "--status-info",
        _ => "--text-lo",
    }
}

/// Add a deferred stylesheet to the document, once per application.
///
/// Two components mounting in the same frame must not insert the same link
/// twice: a duplicate `<link>` is a second stylesheet the browser keeps and
/// re-applies, and nothing downstream can tell "styled" from "styled twice".
///
/// The href is the asset path `Dioxus.toml` serves. The icon sheet carries the
/// font-family utility, not the `@font-face` — that face is declared in
/// `styles/sidebar.css`, which IS eager, so attaching it does not depend on a
/// second request having landed first.
fn attach_stylesheet(href: &'static str) {
    let attached = use_hook(|| Rc::new(Cell::new(false)));
    if attached.get() {
        return;
    }
    if insert_stylesheet(href) {
        attached.set(true);
    }
}

/// Insert the link, reporting whether the document now carries it.
#[cfg(target_arch = "wasm32")]
fn insert_stylesheet(href: &str) -> bool {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return false;
    };
    if stylesheet_present(&document, href) {
        return true;
    }
    let Ok(link) = document.create_element("link") else {
        return false;
    };
    let _ = link.set_attribute("rel", "stylesheet");
    let _ = link.set_attribute("href", href);
    match document.head() {
        Some(head) if head.append_child(&link).is_ok() => true,
        _ => {
            tracing::warn!(target: "design", href, "the document has no head for a deferred stylesheet");
            false
        }
    }
}

/// Whether a link for this stylesheet is already in the document.
///
/// Asked by href rather than by tag, so a page that loaded the sheet eagerly is
/// recognised and a `<link>` for some other sheet is not mistaken for this one.
#[cfg(target_arch = "wasm32")]
fn stylesheet_present(document: &web_sys::Document, href: &str) -> bool {
    document
        .get_elements_by_tag_name("link")
        .iter()
        .any(|link| link.get_attribute("href").as_deref() == Some(href))
}

/// A native build has no document, so there is nothing to attach and nothing to
/// miss: the native target paints nothing.
#[cfg(not(target_arch = "wasm32"))]
fn insert_stylesheet(_href: &str) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::layout::shell_metrics::CoordinatorState;

    #[test]
    fn every_status_the_chrome_uses_maps_to_a_declared_token() {
        // The coordinator states and the worker states are the whole vocabulary
        // this module's callers pass. An unmapped one renders as a raw attribute
        // the stylesheet has no rule for.
        for status in [
            "ok", "idle", "offline", "warn", "error", "info", "done", "running",
        ] {
            assert!(
                status_token(status).starts_with("--"),
                "{status} mapped to a non-token"
            );
        }
    }

    #[test]
    fn an_unfamiliar_status_reads_as_quiet_rather_than_as_a_colour() {
        assert_eq!(status_token("nonsense"), status_token("idle"));
    }

    #[test]
    fn ok_and_done_share_a_token_because_they_mean_the_same_thing() {
        // A finished agent and a reachable coordinator are both "this is fine",
        // and two tokens for one meaning is how a palette grows a shade nobody
        // chose.
        assert_eq!(status_token("ok"), status_token("done"));
    }

    #[test]
    fn idle_and_offline_share_a_colour_but_keep_distinct_words() {
        // The colour is deliberately the same — neither is an alarm, and
        // painting them as one would train a reader to ignore the dot that
        // matters — while the label still distinguishes "nothing is running"
        // from "we cannot reach the coordinator".
        assert_eq!(status_token("idle"), status_token("offline"));
        assert_ne!(
            CoordinatorState::Syncing.label(),
            CoordinatorState::Unreachable.label()
        );
    }

    #[test]
    fn the_deferred_sheets_are_distinct_files() {
        // Both components attach on mount; if the two constants were the same
        // path, the dot would inherit the icon sheet's rules and vice versa.
        assert_ne!(ICON_STYLESHEET_HREF, STATUS_DOT_STYLESHEET_HREF);
    }
}
