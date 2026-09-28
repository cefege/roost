//! The md stylesheets a primitive attaches when it first renders, and the one
//! hook that attaches them. Called by `Icon`, `EmptyState` and `Dialog`; depends
//! on the DOM only inside the wasm adapter below.
//!
//! v2 imported `icon.css`, `EmptyState.css` and `overlays.css` from the component
//! modules that own them (`apps/web/src/components/Settings/md/Icon.tsx`,
//! `EmptyState.tsx`, `Dialog.tsx`), so each sheet arrived with the lazy chunk
//! that first needed it. This build has no chunking, so the component attaches
//! its own sheet on mount, and none of the three is in `Dioxus.toml`'s eager list.

use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;

/// The icon utility sheet: `.md-icon` and its size and fill modifiers.
pub const ICON_STYLESHEET_HREF: &str = "/components/Settings/md/icon.css";

/// The empty-state layout and type styles.
pub const EMPTY_STATE_STYLESHEET_HREF: &str = "/components/Settings/md/EmptyState.css";

/// The dialog and sheet presentation every modal shares.
pub const OVERLAYS_STYLESHEET_HREF: &str = "/components/Settings/md/overlays.css";

/// Add a deferred stylesheet to the document, once per application.
///
/// A hook, so it runs at the top of the component that owns the sheet. Two
/// components mounting in the same frame must not insert the same link twice: a
/// duplicate `<link>` is a second stylesheet the browser keeps and re-applies,
/// and nothing downstream can tell "styled" from "styled twice". The presence
/// check is by href, so a page that loaded the sheet eagerly is recognised.
pub fn use_md_stylesheet(href: &'static str) {
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
        Some(head) if head.append_child(&link).is_ok() => {
            tracing::debug!(target: "design", href, "deferred stylesheet attached");
            true
        }
        _ => {
            tracing::warn!(target: "design", href, "the document has no head for a deferred stylesheet");
            false
        }
    }
}

/// Whether a link for this stylesheet is already in the document.
#[cfg(target_arch = "wasm32")]
fn stylesheet_present(document: &web_sys::Document, href: &str) -> bool {
    // `HtmlCollection` exposes `length` and `item`, not an iterator, so this
    // walks it by index. A collection that shrinks mid-walk simply yields fewer
    // elements, which for a "is this sheet already there" question is the answer.
    let links = document.get_elements_by_tag_name("link");
    (0..links.length()).any(|index| {
        links
            .item(index)
            .is_some_and(|link| link.get_attribute("href").as_deref() == Some(href))
    })
}

/// A native build has no document, so there is nothing to attach and nothing to
/// miss: the native target paints nothing.
#[cfg(not(target_arch = "wasm32"))]
fn insert_stylesheet(_href: &str) -> bool {
    true
}
