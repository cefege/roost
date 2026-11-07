//! Placement of native overlay slots as non-modal floats or stock modals.
//!
//! A non-modal overlay is absolutely positioned without a focus trap or inert
//! content; caret/node anchors carry data attributes for the parent's caret
//! query. Modal slots are stock Dialogs above the covered surface, and only the
//! highest one receives the host-service prompts.

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::components::ui::{Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle};

use super::host_services::OmpHostServicePrompts;
use super::overlay::{
    NativeLayerMarker, OverlayAnchor, OverlaySide, OverlaySize, OverlaySlot, highest_modal,
};

/// Non-modal overlays anchored top or center, inside the surface content.
pub(super) fn non_modal_surface(
    slots: &[OverlaySlot],
    surface_id: &str,
    occluded: bool,
) -> Element {
    if occluded {
        return rsx! {};
    }
    let top: Vec<&OverlaySlot> = slots
        .iter()
        .filter(|slot| !slot.shape.modal && slot.shape.anchor == OverlayAnchor::Top)
        .collect();
    let center: Vec<&OverlaySlot> = slots
        .iter()
        .filter(|slot| !slot.shape.modal && slot.shape.anchor == OverlayAnchor::Center)
        .collect();
    if top.is_empty() && center.is_empty() {
        return rsx! {};
    }
    rsx! {
        if !top.is_empty() {
            div { class: "pointer-events-none absolute inset-x-0 top-0 z-50 flex justify-center p-2",
                for slot in top { {non_modal_child(slot, surface_id)} }
            }
        }
        if !center.is_empty() {
            div { class: "pointer-events-none absolute inset-0 z-50 flex items-center justify-center p-4",
                for slot in center { {non_modal_child(slot, surface_id)} }
            }
        }
    }
}

/// Non-modal overlays anchored to the composer/caret, above the dock.
// Non-modal overlays anchored to the composer/caret, above the dock.
pub(super) fn non_modal_dock(slots: &[OverlaySlot], surface_id: &str, occluded: bool) -> Element {
    if occluded {
        return rsx! {};
    }
    let docked: Vec<&OverlaySlot> = slots
        .iter()
        .filter(|slot| {
            !slot.shape.modal
                && !matches!(
                    &slot.shape.anchor,
                    OverlayAnchor::Top | OverlayAnchor::Center
                )
        })
        .collect();
    if docked.is_empty() {
        return rsx! {};
    }
    // Caret/node-anchored overlays (the autocomplete) flow inline above the
    // dock's editor, like the terminal draws them in the composer; center and
    // bottom-anchored ones stay floating popups above the dock.
    // All non-modal dock overlays flow inline above the dock's editor.
    let elements = docked
        .into_iter()
        .map(|slot| non_modal_child(slot, surface_id))
        .collect::<Vec<_>>();
    rsx! {
        div { class: "flex w-full flex-col items-center gap-2 pb-2",
            for element in elements { {element} }
        }
    }
}

fn non_modal_child(slot: &OverlaySlot, surface_id: &str) -> Element {
    let (anchor, anchor_node) = anchor_attributes(&slot.shape.anchor);
    let content = slot.content.clone();
    let head = slot.shape.head.clone().filter(|_| slot.head_visible);
    let id = slot.id.clone();
    let surface = surface_id.to_owned();
    rsx! {
        div {
            key: "{slot.id}",
            class: "pointer-events-auto w-full max-w-full",
            "data-omp-overlay": "nonmodal",
            "data-omp-overlay-id": id,
            "data-omp-surface": surface,
            "data-omp-anchor": anchor,
            "data-omp-anchor-node": anchor_node,
            div { class: "max-h-96 overflow-y-auto overscroll-contain",
                if let Some(head) = head {
                    p { class: "px-2 pt-1 pb-0.5 text-xs font-medium text-muted-foreground", "{head}" }
                }
                {content}
            }
        }
    }
}

/// One stock Dialog per modal slot; the highest one owns the prompts.
pub(super) fn modal_dialogs(
    slots: &[OverlaySlot],
    show_prompts: bool,
    send: EventHandler<Value>,
) -> Vec<Element> {
    let owner = highest_modal(slots);
    slots
        .iter()
        .enumerate()
        .filter(|(_, slot)| slot.shape.modal)
        .map(|(index, slot)| modal_dialog(slot, show_prompts && owner == Some(index), send))
        .collect()
}

fn modal_dialog(slot: &OverlaySlot, show_prompts: bool, send: EventHandler<Value>) -> Element {
    let content = slot.content.clone();
    let title = slot.title.clone();
    let visible = slot.head_visible;
    let fullscreen = slot.shape.fullscreen;
    let panel = if fullscreen {
        "flex min-h-0 flex-col overflow-auto pt-12".to_owned()
    } else {
        let width = match slot.shape.size {
            OverlaySize::Sm => "max-w-sm",
            OverlaySize::Lg => "max-w-3xl",
            _ => "max-w-lg",
        };
        format!("flex min-h-0 flex-col overflow-auto pt-12 {width}")
    };
    let header_class = if visible { "" } else { "sr-only" };
    rsx! {
        Dialog {
            key: "{slot.id}",
            open: true,
            on_open_change: move |open: bool| {
                if !open {
                    send.call(json!({"method":"input","params":{"data":"\x1b"}}));
                }
            },
            DialogContent {
                class: Some(panel),
                style: fullscreen.then(|| "position:absolute;height:100%;".to_owned()),
                fullscreen,
                close_on_backdrop_click: true,
                DialogHeader { class: Some(header_class.into()),
                    DialogTitle { "{title}" }
                    DialogDescription { class: Some("sr-only".into()), "OMP overlay" }
                }
                if show_prompts {
                    OmpHostServicePrompts {}
                }
                {content}
            }
        }
    }
}

/// The overlay node's own anchor for scroll and pointer targets.
pub(super) fn layer_marker_element(marker: &NativeLayerMarker, surface_id: &str) -> Element {
    let id = marker.id.clone();
    let role = marker.role.clone();
    let surface = surface_id.to_owned();
    rsx! {
        div {
            class: "contents",
            "data-omp-node": id,
            "data-omp-surface": surface,
            "data-omp-role": role,
        }
    }
}

fn anchor_attributes(anchor: &OverlayAnchor) -> (&'static str, String) {
    match anchor {
        OverlayAnchor::Center => ("center", String::new()),
        OverlayAnchor::Top => ("top", String::new()),
        OverlayAnchor::Bottom => ("bottom", String::new()),
        OverlayAnchor::Node { id, side } => (
            match side {
                OverlaySide::Above => "node-above",
                OverlaySide::Below => "node-below",
            },
            id.clone(),
        ),
        OverlayAnchor::Caret { id } => ("caret", id.clone()),
    }
}
