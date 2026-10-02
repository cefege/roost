//! Mounting a subtree outside the element it is rendered in, as v2's Solid
//! `<Portal>` moves the compact composer dock and the terminal key pad to
//! `<body>`. A `position: fixed` box under a transformed, scrollable ancestor —
//! the terminal deck — is placed and scrolled by that ancestor; one under the
//! portal root is placed by the viewport. `app.rs` renders [`PortalRoot`] once;
//! [`Portal`] moves its subtree there on mount and removes it on drop.

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

use dioxus::prelude::*;

/// The id of the one element every portal mounts into.
pub const PORTAL_ROOT_ID: &str = "roost-portal-root";

/// Where portaled subtrees live: inside the Dioxus root, because the renderer
/// delegates bubbling events to that root and a subtree moved outside it would
/// never hear a click; outside every transformed or scrolled surface.
#[component]
pub fn PortalRoot() -> Element {
    rsx! {
        div { id: PORTAL_ROOT_ID, class: "roost-portal-root" }
    }
}

/// Render `children` under [`PortalRoot`] instead of in place.
///
/// The anchor stays where the portal is rendered, so the renderer's own edits
/// around it — a sibling appearing, the portal being replaced — land in the
/// tree it rendered into. Only the inner wrapper moves, and the drop removes
/// it, because the renderer removes the anchor and no longer contains it.
#[cfg(target_arch = "wasm32")]
#[component]
pub fn Portal(children: Element) -> Element {
    let moved: PortaledElement = use_hook(|| Rc::new(RefCell::new(None)));
    let held = moved.clone();
    use_drop(move || {
        if let Some(element) = held.borrow_mut().take() {
            element.remove();
        }
    });
    rsx! {
        div { class: "roost-portal-anchor",
            div {
                class: "roost-portal",
                onmounted: move |event: MountedEvent| move_into_root(&event.data(), &moved),
                {children}
            }
        }
    }
}

/// Natively there is no document to move into, so the subtree renders in place
/// and the mutation stream is the one its children alone produce.
#[cfg(not(target_arch = "wasm32"))]
#[component]
pub fn Portal(children: Element) -> Element {
    rsx! {
        {children}
    }
}

#[cfg(target_arch = "wasm32")]
type PortaledElement = Rc<RefCell<Option<web_sys::Element>>>;

#[cfg(target_arch = "wasm32")]
fn move_into_root(mounted: &MountedData, moved: &PortaledElement) {
    use dioxus::web::WebEventExt as _;

    let Some(element) = mounted.try_as_web_event() else {
        return;
    };
    let Some(root) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(PORTAL_ROOT_ID))
    else {
        tracing::warn!(target: "layout", "no portal root is mounted; the subtree stays in place");
        return;
    };
    if let Err(error) = root.append_child(&element) {
        tracing::warn!(target: "layout", ?error, "the portal root refused the subtree");
        return;
    }
    *moved.borrow_mut() = Some(element);
}
