//! The composer textarea side of the selection handoff: which input is
//! mounted and focused, reading and writing its own selection, detaching its
//! listeners, and the two deferral primitives the restores are ordered with.
//! A file split of `compose_dom`, whose `ComposeShared` this extends. Ports the
//! textarea and timer half of v2's `apps/web/src/renderer/terminalComposeSelection.ts`.

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::Node;

use super::ComposeShared;
use crate::input::compose_selection::{ComposerSelection, SelectionDirection};

impl ComposeShared {
    /// The mounted input's id when it is connected and the active element.
    pub(super) fn focused_input_id(&self) -> Option<u32> {
        let input = self.input.try_borrow().ok()?;
        let mounted = input.as_ref()?;
        let document = self.document()?;
        let element: &Node = mounted.element.as_ref();
        let active = document.active_element()?;
        let active: &Node = active.as_ref();
        (mounted.element.is_connected() && active.is_same_node(Some(element))).then_some(mounted.id)
    }

    pub(super) fn mounted_input_id(&self) -> Option<u32> {
        self.input
            .try_borrow()
            .ok()?
            .as_ref()
            .map(|mounted| mounted.id)
    }

    pub(super) fn remember_composer_selection(&self) {
        let Some(selection) = self.input.try_borrow().ok().and_then(|input| {
            let element = &input.as_ref()?.element;
            Some(ComposerSelection {
                start: element.selection_start().ok().flatten().unwrap_or(0),
                end: element.selection_end().ok().flatten().unwrap_or(0),
                direction: match element.selection_direction().ok().flatten().as_deref() {
                    Some("forward") => SelectionDirection::Forward,
                    Some("backward") => SelectionDirection::Backward,
                    _ => SelectionDirection::None,
                },
            })
        }) else {
            return;
        };
        if let Ok(mut compose) = self.compose.try_borrow_mut() {
            compose.remember_composer_selection(selection);
        }
    }

    pub(super) fn write_composer_selection(&self, selection: ComposerSelection) -> bool {
        let Ok(input) = self.input.try_borrow() else {
            return false;
        };
        let Some(mounted) = input.as_ref() else {
            return false;
        };
        let direction = match selection.direction {
            SelectionDirection::Forward => "forward",
            SelectionDirection::Backward => "backward",
            SelectionDirection::None => "none",
        };
        mounted
            .element
            .set_selection_range_with_direction(selection.start, selection.end, direction)
            .is_ok()
    }

    pub(super) fn unmount_input(&self) {
        let Some(mounted) = self
            .input
            .try_borrow_mut()
            .ok()
            .and_then(|mut input| input.take())
        else {
            return;
        };
        for (kind, listener) in &mounted.listeners {
            let _ = mounted
                .element
                .remove_event_listener_with_callback_and_bool(
                    kind,
                    listener.as_ref().unchecked_ref(),
                    true,
                );
        }
        if let Ok(mut compose) = self.compose.try_borrow_mut() {
            compose.forget_input(mounted.id);
        }
    }
}

/// Run `callback` after the current task, the `setTimeout(…, 0)` v2 orders
/// its restores with.
pub(super) fn schedule(callback: impl FnOnce() + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let function: js_sys::Function = Closure::once_into_js(callback).unchecked_into();
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&function, 0);
}

/// Run `callback` on the next microtask.
pub(super) fn queue_microtask(callback: impl FnOnce() + 'static) {
    let Some(window) = web_sys::window() else {
        return;
    };
    let function: js_sys::Function = Closure::once_into_js(callback).unchecked_into();
    window.queue_microtask(&function);
}
