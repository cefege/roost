//! Wire overlay defaults and modal classification regressions.
//! The autocomplete overlay is caret-anchored and non-modal; only an explicit
//! modal overlay or a native picker/prefs sheet may cover and trap focus.

use roost_protocol::wire::agent_chat::semantic_ui::OmpUiNode;
use serde_json::{Value, json};

use super::overlay::{
    OverlayAnchor, OverlaySide, OverlaySize, layer_has_modal, overlay_shape, sheet_shape,
};

fn node(id: &str, kind: &str, props: Value, children: Vec<OmpUiNode>) -> OmpUiNode {
    OmpUiNode {
        id: id.to_owned(),
        k: kind.to_owned(),
        p: props.as_object().cloned().unwrap_or_default(),
        c: children,
    }
}

fn layer(children: Vec<OmpUiNode>) -> OmpUiNode {
    node("layer", "col", json!({}), children)
}

#[test]
fn a_caret_autocomplete_is_non_modal_and_keeps_the_wire_defaults() {
    let autocomplete = node(
        "complete",
        "overlay",
        json!({"anchor": {"caret": "input"}, "role": "omp.autocomplete"}),
        Vec::new(),
    );
    let shape = overlay_shape(&autocomplete);
    assert!(!shape.modal);
    assert_eq!(shape.anchor, OverlayAnchor::Caret { id: "input".into() });
    assert_eq!(shape.size, OverlaySize::Md);
    assert_eq!(shape.role, "omp.autocomplete");
}

#[test]
fn an_explicit_modal_overlay_is_modal_and_keeps_its_head() {
    let login = node(
        "sheet",
        "overlay",
        json!({"anchor": "center", "size": "md", "modal": true, "head": "Sign in"}),
        Vec::new(),
    );
    let shape = overlay_shape(&login);
    assert!(shape.modal);
    assert_eq!(shape.anchor, OverlayAnchor::Center);
    assert_eq!(shape.head.as_deref(), Some("Sign in"));
}

#[test]
fn a_node_anchored_overlay_keeps_its_side() {
    let hint = node(
        "hint",
        "overlay",
        json!({"anchor": {"node": "card", "side": "below"}}),
        Vec::new(),
    );
    assert_eq!(
        overlay_shape(&hint).anchor,
        OverlayAnchor::Node {
            id: "card".into(),
            side: OverlaySide::Below
        }
    );
}

#[test]
fn a_native_picker_is_a_modal_sheet_and_screen_fills() {
    let models = node(
        "models",
        "picker",
        json!({"title": "Models", "size": "screen"}),
        Vec::new(),
    );
    let shape = sheet_shape(&models);
    assert!(shape.modal);
    assert!(shape.sheet);
    assert!(shape.fullscreen);
    assert_eq!(shape.size, OverlaySize::Full);
}

#[test]
fn an_autocomplete_layer_is_not_a_modal_until_a_picker_joins_it() {
    let autocomplete = node(
        "complete",
        "overlay",
        json!({"anchor": {"caret": "input"}, "role": "omp.autocomplete"}),
        Vec::new(),
    );
    let only_complete = layer(vec![autocomplete.clone()]);
    assert!(!layer_has_modal(Some(&only_complete)));

    let models = node("models", "picker", json!({"title": "Models"}), Vec::new());
    let with_models = layer(vec![autocomplete, models]);
    assert!(layer_has_modal(Some(&with_models)));
}

#[test]
fn a_hidden_modal_does_not_count_as_open() {
    let hidden = node(
        "sheet",
        "overlay",
        json!({"modal": true, "hidden": true}),
        Vec::new(),
    );
    assert!(!layer_has_modal(Some(&layer(vec![hidden]))));
}
