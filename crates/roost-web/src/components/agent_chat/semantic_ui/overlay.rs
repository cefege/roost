//! Native overlay shapes and layer parsing for the browser surface.
//! Layer children retain their structural wrappers and native z-order.
//! Explicit overlay, picker and prefs nodes are placement boundaries; ordinary
//! columns keep their identity, layout props and reveal targets.

use std::collections::BTreeMap;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode, OmpUiSurface};
use serde_json::Value;

use super::nodes::render_node_with_focus;
use super::prop_text;

#[path = "overlay_options.rs"]
pub(super) mod options;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlaySize {
    Sm,
    Md,
    Lg,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverlaySide {
    Above,
    Below,
}

/// The wire `TspOverlayAnchor`, with the terminal's center default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum OverlayAnchor {
    Center,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    TopCenter,
    BottomLeft,
    BottomRight,
    BottomCenter,
    LeftCenter,
    RightCenter,
    Node { id: String, side: OverlaySide },
    Caret { id: String },
}

#[derive(Clone, PartialEq)]
pub(super) struct OverlayShape {
    pub anchor: OverlayAnchor,
    pub size: OverlaySize,
    pub modal: bool,
    pub fullscreen: bool,
    pub head: Option<String>,
    pub role: String,
    pub sheet: bool,
    pub options: options::OverlayOptions,
}

#[derive(Clone, PartialEq)]
pub(super) struct OverlaySlot {
    pub id: String,
    pub content: Element,
    pub surface_id: String,
    pub shape: OverlayShape,
    pub title: String,
    pub head_visible: bool,
    pub head_content: Option<Value>,
    pub boundary: bool,
    pub children: Vec<OverlaySlot>,
    pub group: Option<OmpUiNode>,
}

#[derive(Clone, PartialEq, Eq)]
pub(super) struct NativeLayerMarker {
    pub id: String,
    pub role: String,
}

/// The `layer` node's own anchor, kept even when its children are split.
pub(super) fn layer_marker(layer: Option<&OmpUiNode>) -> Option<NativeLayerMarker> {
    layer
        .filter(|node| !hidden(node))
        .map(|node| NativeLayerMarker {
            id: node.id.clone(),
            role: node
                .p
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
        })
}

/// Build one slot per visible layer child, in native z-order.
pub(super) fn layer_slots(
    layer: Option<&OmpUiNode>,
    surface: &OmpUiSurface,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
) -> Vec<OverlaySlot> {
    let Some(layer) = layer else {
        return Vec::new();
    };
    if hidden(layer) {
        return Vec::new();
    }
    let mut slots = Vec::new();
    for child in &layer.c {
        push_slot(child, surface, send, blobs, &mut slots);
    }
    slots
}

fn push_slot(
    node: &OmpUiNode,
    surface: &OmpUiSurface,
    send: &EventHandler<Value>,
    blobs: &BTreeMap<String, OmpUiBlob>,
    slots: &mut Vec<OverlaySlot>,
) {
    if hidden(node) {
        return;
    }
    let mut children = Vec::new();
    if node.k == "col" {
        for child in &node.c {
            push_slot(child, surface, send, blobs, &mut children);
        }
    }
    if node.k == "overlay" && node.c.is_empty() {
        return;
    }
    let shape = match node.k.as_str() {
        "picker" | "prefs" => sheet_shape(node),
        _ => overlay_shape(node),
    };
    let title = shape
        .head
        .clone()
        .or_else(|| prop_text(&node.p, "title"))
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            if shape.role.is_empty() {
                "OMP overlay".to_owned()
            } else {
                shape.role.clone()
            }
        });
    let content = if node.k == "col" {
        let body = super::layout::render_box(
            node,
            &surface.id,
            *send,
            blobs,
            children
                .iter()
                .map(|child| super::overlay_render::nested_non_modal(child, &surface.id)),
        );
        super::chrome::decorate(node, &surface.id, *send, body)
    } else {
        render_node_with_focus(node, &surface.id, send, blobs, surface.focus.as_deref())
    };
    slots.push(OverlaySlot {
        id: node.id.clone(),
        surface_id: surface.id.clone(),
        content,
        head_visible: !shape.sheet && shape.head.is_some(),
        shape,
        title,
        head_content: node.p.get("head").cloned(),
        boundary: matches!(node.k.as_str(), "overlay" | "picker" | "prefs"),
        children,
        group: (node.k == "col").then(|| OmpUiNode {
            id: node.id.clone(),
            k: node.k.clone(),
            p: node.p.clone(),
            c: Vec::new(),
        }),
    });
}

/// True when any layer child is an actual modal, never an autocomplete.
pub(super) fn layer_has_modal(layer: Option<&OmpUiNode>) -> bool {
    layer.is_some_and(|layer| !hidden(layer) && layer.c.iter().any(modal_child))
}

/// Whether this surface is a screen or currently holds an actual modal layer.
pub(super) fn surface_has_modal(surface: &OmpUiSurface) -> bool {
    surface.mode == "screen"
        || layer_has_modal(surface.root.c.iter().find(|node| node.id == "layer"))
}

fn modal_child(node: &OmpUiNode) -> bool {
    if hidden(node) {
        return false;
    }
    match node.k.as_str() {
        "overlay" => {
            !node.c.is_empty()
                && node
                    .p
                    .get("modal")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
        }
        "picker" | "prefs" => true,
        "col" => node.c.iter().any(modal_child),
        _ => false,
    }
}

pub(super) fn has_modal_layer(slots: &[OverlaySlot]) -> bool {
    slots
        .iter()
        .any(|slot| slot.shape.modal || has_modal_layer(&slot.children))
}

pub(super) fn modal_slots(slots: &[OverlaySlot]) -> Vec<&OverlaySlot> {
    let mut modals = Vec::new();
    for slot in slots {
        if slot.shape.modal {
            modals.push(slot);
        } else {
            modals.extend(modal_slots(&slot.children));
        }
    }
    modals
}

fn hidden(node: &OmpUiNode) -> bool {
    node.p.get("hidden").and_then(Value::as_bool) == Some(true)
}

pub(super) fn overlay_shape(node: &OmpUiNode) -> OverlayShape {
    let size = parse_size(node.p.get("size"));
    OverlayShape {
        anchor: parse_anchor(node.p.get("anchor")),
        size,
        modal: node
            .p
            .get("modal")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        fullscreen: size == OverlaySize::Full,
        head: prop_text(&node.p, "head"),
        role: node
            .p
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        sheet: false,
        options: options::OverlayOptions::parse(&node.p),
    }
}

pub(super) fn sheet_shape(node: &OmpUiNode) -> OverlayShape {
    // OMP's settings and advisor panels own the whole terminal frame, so the
    // browser sheet is fullscreen rather than sized to its rows.
    let size = if node.k == "picker" {
        parse_size(node.p.get("size"))
    } else {
        OverlaySize::Full
    };
    OverlayShape {
        anchor: OverlayAnchor::Center,
        size,
        modal: true,
        fullscreen: size == OverlaySize::Full,
        head: prop_text(&node.p, "head").or_else(|| prop_text(&node.p, "title")),
        role: node
            .p
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        sheet: true,
        options: options::OverlayOptions::default(),
    }
}

fn parse_size(value: Option<&Value>) -> OverlaySize {
    match value.and_then(Value::as_str) {
        Some("sm") => OverlaySize::Sm,
        Some("lg") => OverlaySize::Lg,
        Some("full") | Some("screen") => OverlaySize::Full,
        _ => OverlaySize::Md,
    }
}

fn parse_anchor(value: Option<&Value>) -> OverlayAnchor {
    match value {
        Some(Value::String(name)) => match name.as_str() {
            "top" => OverlayAnchor::Top,
            "bottom" => OverlayAnchor::Bottom,
            "top-left" => OverlayAnchor::TopLeft,
            "top-right" => OverlayAnchor::TopRight,
            "top-center" => OverlayAnchor::TopCenter,
            "bottom-left" => OverlayAnchor::BottomLeft,
            "bottom-right" => OverlayAnchor::BottomRight,
            "bottom-center" => OverlayAnchor::BottomCenter,
            "left-center" => OverlayAnchor::LeftCenter,
            "right-center" => OverlayAnchor::RightCenter,
            _ => OverlayAnchor::Center,
        },
        Some(Value::Object(anchor)) => {
            if let Some(caret) = anchor.get("caret").and_then(Value::as_str) {
                return OverlayAnchor::Caret {
                    id: caret.to_owned(),
                };
            }
            if let Some(node) = anchor.get("node").and_then(Value::as_str) {
                let side = match anchor.get("side").and_then(Value::as_str) {
                    Some("below") => OverlaySide::Below,
                    _ => OverlaySide::Above,
                };
                return OverlayAnchor::Node {
                    id: node.to_owned(),
                    side,
                };
            }
            OverlayAnchor::Center
        }
        _ => OverlayAnchor::Center,
    }
}
