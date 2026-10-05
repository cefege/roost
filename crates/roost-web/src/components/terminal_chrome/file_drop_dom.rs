//! The document listeners that apply `file_drop`'s rules: the page-wide guard
//! that keeps a dropped file or link from navigating the tab, installed once by
//! the app root, and one terminal pane's drop target, installed for the pane's
//! lifetime by `terminal_drop_target`. A dropped file is read by the attach
//! button's own reader and handed to the same upload callback the button feeds.

#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
use std::rc::Rc;

use dioxus::prelude::*;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;

use super::attachment_picker::ChosenFile;
#[cfg(target_arch = "wasm32")]
use super::attachment_picker::spawn_read_chosen;
#[cfg(target_arch = "wasm32")]
use super::file_drop::{
    DragKinds, PaneDropAction, page_blocks_default, pane_claims, pane_drop_action, uri_list_text,
};

/// The events the page guard cancels.
#[cfg(target_arch = "wasm32")]
const GUARDED_EVENTS: [&str; 3] = ["dragenter", "dragover", "drop"];

/// The elements whose own default for a dropped link is to insert it.
#[cfg(target_arch = "wasm32")]
const EDITABLE_SELECTOR: &str = "input, textarea, [contenteditable=''], [contenteditable='true'], [contenteditable='plaintext-only']";

/// The pane root `CellTerminal` renders, which names its session.
#[cfg(target_arch = "wasm32")]
const PANE_SELECTOR: &str = "[data-testid='cell-terminal-pane']";

/// The guard listens in the capture phase, ahead of the panes.
#[cfg(target_arch = "wasm32")]
const CAPTURE: bool = true;

/// Panes listen in the bubble phase, after the guard.
#[cfg(target_arch = "wasm32")]
const BUBBLE: bool = false;

/// One registered document listener, removed when its owner drops.
#[cfg(target_arch = "wasm32")]
type DragListener = (&'static str, Closure<dyn FnMut(web_sys::DragEvent)>);

/// The page-wide drop guard. Dropping it removes its listeners.
#[derive(Default)]
pub struct PageDropGuard {
    #[cfg(target_arch = "wasm32")]
    listeners: Vec<DragListener>,
}

impl std::fmt::Debug for PageDropGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PageDropGuard")
    }
}

impl Drop for PageDropGuard {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        remove_listeners(&self.listeners, CAPTURE);
    }
}

/// Keep a file or link dropped anywhere on the page from opening in the tab.
///
/// The guard also marks such a drag as accepting nothing (`dropEffect: none`),
/// so the cursor is honest where no pane takes it. It listens in the CAPTURE
/// phase so it runs before every pane's bubble-phase listener, whatever the
/// mount order: a pane that claims the drag turns it back into a copy.
pub fn install_page_drop_guard() -> PageDropGuard {
    #[cfg(target_arch = "wasm32")]
    {
        let listeners: Vec<DragListener> = GUARDED_EVENTS
            .iter()
            .filter_map(|name| {
                let closure = Closure::<dyn FnMut(web_sys::DragEvent)>::new(guard_page_drag);
                add_listener(name, &closure, CAPTURE).then_some((*name, closure))
            })
            .collect();
        tracing::info!(target: "attachments", "page drop guard installed");
        PageDropGuard { listeners }
    }
    #[cfg(not(target_arch = "wasm32"))]
    PageDropGuard::default()
}

#[cfg(target_arch = "wasm32")]
fn guard_page_drag(event: web_sys::DragEvent) {
    if !page_blocks_default(drag_kinds(&event), target_is_editable(&event)) {
        return;
    }
    event.prevent_default();
    if event.type_() != "drop" {
        set_drop_effect(&event, "none");
    }
}

/// What one pane's drop target reports to, and reads from, its pane.
#[derive(Clone)]
pub struct PaneDropTarget {
    /// The session the pane shows; matched against the pane under the pointer.
    pub session_id: String,
    /// Whether the pane is the focused one, which takes drops over no pane.
    pub focused: Rc<dyn Fn() -> bool>,
    /// The attach button's upload callback.
    pub on_files: EventHandler<Vec<ChosenFile>>,
    /// Paste a dropped URL into the pane.
    pub on_link: Rc<dyn Fn(&str)>,
    /// Whether a file drag this pane would take is over the page.
    pub on_hover: Rc<dyn Fn(bool)>,
}

impl std::fmt::Debug for PaneDropTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneDropTarget")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

/// One pane's drop listeners. Dropping it removes them.
#[derive(Default)]
pub struct PaneDropListeners {
    #[cfg(target_arch = "wasm32")]
    listeners: Vec<DragListener>,
}

impl std::fmt::Debug for PaneDropListeners {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PaneDropListeners")
    }
}

impl Drop for PaneDropListeners {
    fn drop(&mut self) {
        #[cfg(target_arch = "wasm32")]
        remove_listeners(&self.listeners, BUBBLE);
    }
}

/// The drag state one pane's listeners share: how deep the pointer is in the
/// page's elements, and whether the drag began inside this page.
#[cfg(target_arch = "wasm32")]
#[derive(Default)]
struct DragTrack {
    depth: Cell<u32>,
    internal: Cell<bool>,
}

/// One drag event's handler, given the pane and its shared drag state.
#[cfg(target_arch = "wasm32")]
type PaneDragHandler = fn(&PaneDropTarget, &DragTrack, &web_sys::DragEvent);

/// Listen on the document for drags this pane takes.
///
/// `dragenter`/`dragleave` pair per element, so their running depth reaches
/// zero only when the drag leaves the window or is cancelled; that, a drop, or
/// a `dragend` is what clears the hover.
pub fn install_pane_drop_listeners(target: PaneDropTarget) -> PaneDropListeners {
    #[cfg(target_arch = "wasm32")]
    {
        let track = Rc::new(DragTrack::default());
        let handlers: [(&'static str, PaneDragHandler); 6] = [
            ("dragstart", |_, track, _| track.internal.set(true)),
            ("dragend", |target, track, _| {
                track.internal.set(false);
                track.depth.set(0);
                (target.on_hover)(false);
            }),
            ("dragenter", |target, track, event| {
                track.depth.set(track.depth.get().saturating_add(1));
                accept_drag_over(target, track, event);
            }),
            ("dragover", accept_drag_over),
            ("dragleave", |target, track, _| {
                track.depth.set(track.depth.get().saturating_sub(1));
                if track.depth.get() == 0 {
                    (target.on_hover)(false);
                }
            }),
            ("drop", take_drop),
        ];
        let listeners = handlers
            .into_iter()
            .filter_map(|(name, on_event)| {
                let target = target.clone();
                let track = Rc::clone(&track);
                let closure = Closure::<dyn FnMut(web_sys::DragEvent)>::new(
                    move |event: web_sys::DragEvent| on_event(&target, &track, &event),
                );
                add_listener(name, &closure, BUBBLE).then_some((name, closure))
            })
            .collect();
        PaneDropListeners { listeners }
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = target;
        PaneDropListeners::default()
    }
}

/// Accept a drag this pane would take, and say whether its files are over it.
#[cfg(target_arch = "wasm32")]
fn accept_drag_over(target: &PaneDropTarget, track: &DragTrack, event: &web_sys::DragEvent) {
    let action = claimed_action(target, track, event);
    if action != PaneDropAction::Decline {
        event.prevent_default();
        set_drop_effect(event, "copy");
    }
    (target.on_hover)(action == PaneDropAction::Upload);
}

/// Take a drop this pane claims: files go to the attach button's upload, a
/// link is pasted.
#[cfg(target_arch = "wasm32")]
fn take_drop(target: &PaneDropTarget, track: &DragTrack, event: &web_sys::DragEvent) {
    let action = claimed_action(target, track, event);
    track.depth.set(0);
    (target.on_hover)(false);
    let Some(transfer) = event.data_transfer() else {
        return;
    };
    match action {
        PaneDropAction::Upload => {
            event.prevent_default();
            let files: Vec<web_sys::File> = transfer
                .files()
                .map(|list| (0..list.length()).filter_map(|idx| list.get(idx)).collect())
                .unwrap_or_default();
            tracing::info!(
                target: "attachments",
                session_id = %target.session_id,
                files = files.len(),
                "files dropped on a terminal pane"
            );
            spawn_read_chosen(files, target.on_files);
        }
        PaneDropAction::PasteLink => {
            event.prevent_default();
            let raw = transfer.get_data("text/uri-list").unwrap_or_default();
            if let Some(text) = uri_list_text(&raw) {
                tracing::info!(
                    target: "attachments",
                    session_id = %target.session_id,
                    "a link dropped on a terminal pane was pasted"
                );
                (target.on_link)(&text);
            }
        }
        PaneDropAction::Decline => {}
    }
}

/// This pane's action for the drag, or `Decline` when another pane owns it.
#[cfg(target_arch = "wasm32")]
fn claimed_action(
    target: &PaneDropTarget,
    track: &DragTrack,
    event: &web_sys::DragEvent,
) -> PaneDropAction {
    let element = target_element(event);
    let over_pane = element
        .as_ref()
        .and_then(|element| element.closest(PANE_SELECTOR).ok().flatten())
        .and_then(|pane| pane.get_attribute("data-session-id"));
    if !pane_claims(over_pane.as_deref(), &target.session_id, (target.focused)()) {
        return PaneDropAction::Decline;
    }
    pane_drop_action(
        drag_kinds(event),
        target_is_editable(event),
        track.internal.get(),
    )
}

#[cfg(target_arch = "wasm32")]
fn drag_kinds(event: &web_sys::DragEvent) -> DragKinds {
    let Some(transfer) = event.data_transfer() else {
        return DragKinds::default();
    };
    DragKinds::from_types(transfer.types().iter().filter_map(|name| name.as_string()))
}

#[cfg(target_arch = "wasm32")]
fn target_element(event: &web_sys::DragEvent) -> Option<web_sys::Element> {
    let node = event.target()?.dyn_into::<web_sys::Node>().ok()?;
    match node.dyn_ref::<web_sys::Element>() {
        Some(element) => Some(element.clone()),
        None => node.parent_element(),
    }
}

#[cfg(target_arch = "wasm32")]
fn target_is_editable(event: &web_sys::DragEvent) -> bool {
    target_element(event)
        .and_then(|element| element.closest(EDITABLE_SELECTOR).ok().flatten())
        .is_some()
}

#[cfg(target_arch = "wasm32")]
fn set_drop_effect(event: &web_sys::DragEvent, effect: &str) {
    if let Some(transfer) = event.data_transfer() {
        transfer.set_drop_effect(effect);
    }
}

#[cfg(target_arch = "wasm32")]
fn add_listener(
    name: &str,
    closure: &Closure<dyn FnMut(web_sys::DragEvent)>,
    capture: bool,
) -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_some_and(|document| {
            document
                .add_event_listener_with_callback_and_bool(
                    name,
                    closure.as_ref().unchecked_ref(),
                    capture,
                )
                .is_ok()
        })
}

#[cfg(target_arch = "wasm32")]
fn remove_listeners(listeners: &[DragListener], capture: bool) {
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    for (name, closure) in listeners {
        let _ = document.remove_event_listener_with_callback_and_bool(
            name,
            closure.as_ref().unchecked_ref(),
            capture,
        );
    }
}
