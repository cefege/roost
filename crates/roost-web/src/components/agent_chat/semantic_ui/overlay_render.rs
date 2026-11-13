//! Measured native floats and stock modal overlays above a surface.
//! Non-modal layers never enter the composer's flow or take its focus.
//! Browser placement follows node/caret geometry and visible viewport bounds;
//! stock Dialog retains cancellation, focus restoration and modal stacking.

use dioxus::prelude::*;
use serde_json::{Value, json};

use crate::components::ui::{Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle};

use super::host_services::OmpHostServicePrompts;
use super::overlay::{
    NativeLayerMarker, OverlayAnchor, OverlaySide, OverlaySize, OverlaySlot, modal_slots,
};

#[cfg(target_arch = "wasm32")]
#[path = "overlay_browser.rs"]
mod browser;
#[cfg(any(target_arch = "wasm32", test))]
#[path = "overlay_geometry.rs"]
pub(super) mod geometry;

/// All non-modal slots share the surface plane, regardless of anchor location.
pub(super) fn non_modal_surface(
    slots: &[OverlaySlot],
    surface_id: &str,
    occluded: bool,
) -> Element {
    if occluded {
        return rsx! {};
    }
    rsx! {
        for slot in slots.iter().filter(|slot| !slot.shape.modal) {
            {non_modal_child(slot, surface_id)}
        }
    }
}

pub(super) fn nested_non_modal(slot: &OverlaySlot, surface_id: &str) -> Element {
    if slot.shape.modal {
        rsx! {}
    } else if slot.boundary {
        non_modal_child(slot, surface_id)
    } else {
        slot.content.clone()
    }
}

fn non_modal_child(slot: &OverlaySlot, surface_id: &str) -> Element {
    rsx! { NativeOverlay { key: "{slot.id}", slot: slot.clone(), surface_id: surface_id.to_owned() } }
}

#[component]
fn NativeOverlay(slot: OverlaySlot, surface_id: String) -> Element {
    let element_id = use_hook(|| crate::components::ui::field::scoped_element_id("omp-overlay"));
    #[cfg(target_arch = "wasm32")]
    browser::use_overlay_placement(&element_id, &surface_id, &slot.shape, false);
    let (anchor, anchor_node) = anchor_attributes(&slot.shape.anchor);
    let size_class = match slot.shape.size {
        OverlaySize::Sm => "w-full max-w-sm",
        OverlaySize::Md => "w-full max-w-lg",
        OverlaySize::Lg => "w-full max-w-3xl",
        OverlaySize::Full => "w-full max-w-full",
    };
    rsx! {
        div {
            id: element_id,
            class: "pointer-events-auto fixed z-50 min-w-0 overflow-y-auto overscroll-contain rounded-md border border-border bg-popover text-popover-foreground shadow-md {size_class}",
            style: "visibility:hidden;",
            "data-omp-overlay": "nonmodal",
            "data-omp-overlay-id": slot.id,
            "data-omp-surface": surface_id,
            "data-omp-anchor": anchor,
            "data-omp-anchor-node": anchor_node,
            {overlay_metrics()}
            if slot.head_visible {
                p { class: "px-2 pt-1 pb-0.5 text-xs font-medium text-muted-foreground",
                    {super::content::render_spans(slot.head_content.as_ref(), "")}
                }
            }
            {slot.content}
        }
    }
}

#[component]
fn ModalPlacement(surface_id: String, shape: super::overlay::OverlayShape) -> Element {
    #[cfg(target_arch = "wasm32")]
    {
        let context = use_context::<crate::components::ui::dialog::DialogContext>();
        browser::use_overlay_placement(&context.content_id, &surface_id, &shape, true);
    }
    #[cfg(not(target_arch = "wasm32"))]
    let _ = (surface_id, shape);
    overlay_metrics()
}

fn overlay_metrics() -> Element {
    rsx! {
        span {
            class: "pointer-events-none invisible absolute",
            style: "width:1ch;height:1lh;",
            "data-omp-overlay-metrics": "true",
            aria_hidden: "true",
        }
    }
}

/// One stock Dialog per modal slot; the highest one owns the prompts.
pub(super) fn modal_dialogs(
    slots: &[OverlaySlot],
    show_prompts: bool,
    send: EventHandler<Value>,
) -> Vec<Element> {
    let modals = modal_slots(slots);
    let owner = modals.last().map(|slot| slot.id.as_str());
    slots
        .iter()
        .filter_map(|slot| modal_tree(slot, show_prompts, owner, send))
        .collect()
}

fn modal_tree(
    slot: &OverlaySlot,
    show_prompts: bool,
    owner: Option<&str>,
    send: EventHandler<Value>,
) -> Option<Element> {
    if slot.shape.modal {
        return Some(modal_dialog(
            slot,
            show_prompts && owner == Some(slot.id.as_str()),
            send,
        ));
    }
    let group = slot.group.as_ref()?;
    let children: Vec<_> = slot
        .children
        .iter()
        .filter_map(|child| modal_tree(child, show_prompts, owner, send))
        .collect();
    if children.is_empty() {
        return None;
    }
    let body = super::layout::render_box(
        group,
        &slot.surface_id,
        send,
        &std::collections::BTreeMap::new(),
        children.into_iter(),
    );
    Some(super::chrome::decorate(group, &slot.surface_id, send, body))
}

fn modal_dialog(slot: &OverlaySlot, show_prompts: bool, send: EventHandler<Value>) -> Element {
    let content = slot.content.clone();
    let title = slot.title.clone();
    let visible = slot.head_visible;
    let fullscreen = slot.shape.fullscreen;
    let panel = match (fullscreen, slot.shape.size) {
        (true, _) => "flex min-h-0 flex-col overflow-auto pt-12",
        (_, OverlaySize::Sm) => "flex min-h-0 flex-col overflow-auto pt-12 max-w-sm",
        (_, OverlaySize::Lg) => "flex min-h-0 flex-col overflow-auto pt-12 max-w-3xl",
        _ => "flex min-h-0 flex-col overflow-auto pt-12 max-w-lg",
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
                class: Some(panel.into()),
                style: fullscreen.then(|| "position:absolute;height:100%;".to_owned()),
                fullscreen,
                close_on_backdrop_click: true,
                DialogHeader { class: Some(header_class.into()),
                    DialogTitle {
                        if visible {
                            {super::content::render_spans(slot.head_content.as_ref(), "")}
                        } else {
                            "{title}"
                        }
                    }
                    DialogDescription { class: Some("sr-only".into()), "OMP overlay" }
                }
                ModalPlacement { surface_id: slot.surface_id.clone(), shape: slot.shape.clone() }
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
        OverlayAnchor::TopLeft => ("top-left", String::new()),
        OverlayAnchor::TopRight => ("top-right", String::new()),
        OverlayAnchor::TopCenter => ("top-center", String::new()),
        OverlayAnchor::BottomLeft => ("bottom-left", String::new()),
        OverlayAnchor::BottomRight => ("bottom-right", String::new()),
        OverlayAnchor::BottomCenter => ("bottom-center", String::new()),
        OverlayAnchor::LeftCenter => ("left-center", String::new()),
        OverlayAnchor::RightCenter => ("right-center", String::new()),
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
