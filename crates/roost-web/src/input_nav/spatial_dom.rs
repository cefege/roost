//! The keydown listener that drives directional focus navigation. Installed
//! once from the App; listens in the BUBBLE phase so it is strictly the last
//! claimant on an arrow key: anything with its own arrow handling (the shortcut
//! router, a listbox, a scrolling `.wterm`) has already run and either consumed
//! the key or left it alone. Gathers DOM facts; `spatial` decides.
//! Ported from `apps/web/src/lib/spatialNavigation.ts` (`installSpatialNavigation`).

use dioxus::prelude::{ReadableExt as _, Signal};
use wasm_bindgen::JsCast as _;
use wasm_bindgen::closure::Closure;
use web_sys::{Element, KeyboardEvent, ScrollIntoViewOptions, ScrollLogicalPosition};

use crate::input_nav::dom_read;
use crate::input_nav::modality::NavModality;
use crate::input_nav::spatial::{
    ArrowKeydown, FocusedArrowOwner, NavRect, claimable_direction, pick_target,
};

const FOCUSABLE_SELECTOR: &str = concat!(
    "a[href],button:not([disabled]),input:not([disabled]),select:not([disabled]),",
    "textarea:not([disabled]),[tabindex]:not([tabindex=\"-1\"])"
);

// Roving-focus surfaces own their own arrow handling (context menus, listboxes).
const ROVING_ROLES: &str = "[role=\"menu\"],[role=\"listbox\"],[role=\"combobox\"]";

/// Removes the keydown listener when dropped (App unmount).
pub struct SpatialNavigationGuard {
    listener: Closure<dyn FnMut(KeyboardEvent)>,
}

impl std::fmt::Debug for SpatialNavigationGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SpatialNavigationGuard")
    }
}

impl Drop for SpatialNavigationGuard {
    fn drop(&mut self) {
        if let Some(window) = web_sys::window() {
            let _ = window
                .remove_event_listener_with_callback("keydown", self.listener.as_ref().unchecked_ref());
        }
        tracing::info!(target: "input_nav", "spatial navigation uninstalled");
    }
}

/// Install directional focus navigation for the lifetime of the guard.
pub fn install_spatial_navigation(modality: Signal<NavModality>) -> SpatialNavigationGuard {
    let listener = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        navigate_on_keydown(&modality.peek(), &event);
    });
    // BUBBLE phase, deliberately: in capture phase `defaultPrevented` could only
    // reflect earlier window-capture listeners, so a listbox or slider would be
    // hijacked before it ever saw the key.
    if let Some(window) = web_sys::window() {
        let _ = window.add_event_listener_with_callback("keydown", listener.as_ref().unchecked_ref());
    }
    tracing::info!(target: "input_nav", "spatial navigation installed");
    SpatialNavigationGuard { listener }
}

fn navigate_on_keydown(modality: &NavModality, event: &KeyboardEvent) {
    let key = event.key();
    let keydown = ArrowKeydown {
        key: &key,
        default_prevented: event.default_prevented(),
        modified: event.meta_key() || event.ctrl_key() || event.alt_key() || event.shift_key(),
    };
    let active = dom_read::active_element();
    let owner = active.as_ref().map(arrow_owner);
    let Some(direction) = claimable_direction(modality.directional_input_active(), &keydown, owner.as_ref())
    else {
        return;
    };
    let candidates = collect_candidates(active.as_ref());
    if candidates.is_empty() {
        return;
    }
    let rects: Vec<NavRect> = candidates.iter().map(dom_read::nav_rect).collect();
    let origin = active
        .as_ref()
        .filter(|element| !dom_read::is_body(element))
        .map(dom_read::nav_rect);
    let Some(target) = pick_target(origin.as_ref(), &rects, direction).and_then(|index| candidates.get(index))
    else {
        return;
    };
    event.prevent_default();
    if let Some(html) = dom_read::html(target) {
        let _ = html.focus();
    }
    let options = ScrollIntoViewOptions::new();
    options.set_block(ScrollLogicalPosition::Nearest);
    options.set_inline(ScrollLogicalPosition::Nearest);
    target.scroll_into_view_with_scroll_into_view_options(&options);
    let to = target
        .get_attribute("data-testid")
        .or_else(|| Some(target.id()).filter(|id| !id.is_empty()))
        .unwrap_or_else(|| target.tag_name());
    tracing::info!(target: "input_nav", key = %key, to = %to, "dpad.nav");
}

fn arrow_owner(active: &Element) -> FocusedArrowOwner {
    let tag = active.tag_name();
    let content_editable = dom_read::html(active).is_some_and(|html| html.is_content_editable());
    FocusedArrowOwner {
        editable: tag == "INPUT" || tag == "TEXTAREA" || content_editable,
        in_roving_role: dom_read::closest(active, ROVING_ROLES).is_some(),
        terminal_scroll_box: dom_read::matches(active, ".wterm").then(|| dom_read::scroll_geometry(active)),
    }
}

fn collect_candidates(active: Option<&Element>) -> Vec<Element> {
    let Some(document) = dom_read::document() else {
        return Vec::new();
    };
    let Ok(nodes) = document.query_selector_all(FOCUSABLE_SELECTOR) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index))
        .filter_map(|node| node.dyn_into::<Element>().ok())
        .filter(|element| Some(element) != active)
        .filter(|element| element.get_client_rects().length() > 0)
        .filter(|element| dom_read::closest(element, "[inert],[aria-hidden=\"true\"]").is_none())
        // The PTY textarea is off-screen and consumes every arrow, so landing
        // on it traps focus with no way back; excluding it here holds for both
        // modalities whatever tabIndex the input controller gave it.
        .filter(|element| !element.class_list().contains("terminal-input"))
        .collect()
}
