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

fn grouped_layer_surface(
    surface: roost_protocol::wire::agent_chat::semantic_ui::OmpUiSurface,
) -> dioxus::prelude::Element {
    let send = dioxus::prelude::EventHandler::new(|_: Value| {});
    let slots = super::overlay::layer_slots(
        surface.root.c.first(),
        &surface,
        &send,
        &std::collections::BTreeMap::new(),
    );
    super::overlay_render::non_modal_surface(&slots, &surface.id, false)
}

#[test]
fn grouped_layer_columns_keep_real_identity_and_layout_with_overlay_children() {
    use dioxus::core::{AttributeValue, Mutation};
    use dioxus::prelude::VirtualDom;
    let group = node(
        "group",
        "col",
        json!({"role":"grouped.actions","gap":"md","align":"start","grow":1}),
        vec![
            node("caption", "text", json!({"text":"Actions"}), Vec::new()),
            node(
                "hint",
                "overlay",
                json!({"anchor":{"node":"caption","side":"below"}}),
                vec![node(
                    "hint-text",
                    "text",
                    json!({"text":"Details"}),
                    Vec::new(),
                )],
            ),
        ],
    );
    let root = node("surface", "col", json!({}), vec![layer(vec![group])]);
    let surface = match serde_json::from_value(json!({"id":"surface","sequence":1,"root":root})) {
        Ok(surface) => surface,
        Err(error) => panic!("invalid surface fixture: {error}"),
    };
    let mut dom = VirtualDom::new_with_props(grouped_layer_surface, surface);
    let attributes: Vec<_> = dom
        .rebuild_to_vec()
        .edits
        .into_iter()
        .filter_map(|edit| match edit {
            Mutation::SetAttribute {
                name,
                value: AttributeValue::Text(value),
                ..
            } => Some((name, value)),
            _ => None,
        })
        .collect();
    for id in ["group", "caption", "hint", "hint-text"] {
        assert_eq!(
            attributes
                .iter()
                .filter(|(name, value)| *name == "data-omp-node" && value == id)
                .count(),
            1,
            "{id} must remain one actual target"
        );
    }
    assert!(
        attributes
            .iter()
            .any(|(name, value)| *name == "data-omp-role" && value == "grouped.actions")
    );
    assert!(attributes.iter().any(|(name, value)| *name == "class"
        && value.contains("gap-4")
        && value.contains("items-start")));
    assert!(
        attributes
            .iter()
            .any(|(name, value)| *name == "style" && value.contains("flex-grow:1"))
    );
}
