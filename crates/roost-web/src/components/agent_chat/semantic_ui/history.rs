//! Reuses a native history tree while only the composer or unrelated state changes.
//! The surface still reads every pump revision; node, focus, blobs and sender
//! invalidate the local snapshot. Changed history remains fully rendered.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use dioxus::prelude::*;
use roost_protocol::wire::agent_chat::semantic_ui::{OmpUiBlob, OmpUiNode};
use serde_json::Value;

use super::nodes::render_node_with_focus;
use crate::pump::Pump;

struct RenderedHistory {
    node: OmpUiNode,
    blobs: BTreeMap<String, OmpUiBlob>,
    focus: Option<String>,
    send: EventHandler<Value>,
    element: Element,
}

#[component]
pub(super) fn NativeHistory(
    pump: Pump,
    conversation_id: String,
    surface_id: String,
    node_id: String,
    send: EventHandler<Value>,
) -> Element {
    let _revision = pump.revision()();
    let snapshot = use_hook(|| Rc::new(RefCell::new(None::<RenderedHistory>)));
    let core = pump.core();
    let core = core.borrow();
    let Some(document) = core
        .store()
        .agent_chat
        .transcripts
        .get(&conversation_id)
        .and_then(|loaded| loaded.transcript.omp_ui.as_ref())
    else {
        return rsx! {};
    };
    let Some(surface) = document.surfaces.get(&surface_id) else {
        return rsx! {};
    };
    let Some(node) = surface
        .root
        .c
        .iter()
        .find(|node| node.id == node_id)
        .or_else(|| (surface.root.id == node_id).then_some(&surface.root))
    else {
        return rsx! {};
    };
    let mut snapshot = snapshot.borrow_mut();
    if let Some(previous) = snapshot.as_mut() {
        let node_changed = previous.node != *node;
        let blobs_changed = previous.blobs != document.blobs;
        let focus_changed = previous.focus != surface.focus;
        if !node_changed && !blobs_changed && !focus_changed && previous.send == send {
            return previous.element.clone();
        }
        previous.element = render_node_with_focus(
            node,
            &surface_id,
            &send,
            &document.blobs,
            surface.focus.as_deref(),
        );
        if node_changed {
            previous.node.clone_from(node);
        }
        if blobs_changed {
            previous.blobs.clone_from(&document.blobs);
        }
        if focus_changed {
            previous.focus.clone_from(&surface.focus);
        }
        previous.send = send;
        return previous.element.clone();
    }
    let element = render_node_with_focus(
        node,
        &surface_id,
        &send,
        &document.blobs,
        surface.focus.as_deref(),
    );
    *snapshot = Some(RenderedHistory {
        node: node.clone(),
        blobs: document.blobs.clone(),
        focus: surface.focus.clone(),
        send,
        element: element.clone(),
    });
    element
}
