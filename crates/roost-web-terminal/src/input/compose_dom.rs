//! The DOM half of the composer's selection handoff: the document
//! `selectionchange` listener, the composer textarea's capture-phase edit
//! listeners, and the zero-delay timers and microtasks that order restores
//! after the browser's own default edit. `ComposeSelection` decides and
//! `PaneSelection` reads and writes the terminal's range; this sequences them.
//! The composer (`TerminalComposeButton` in the web app) owns one per dock.
//! Ports the DOM wiring of v2's `apps/web/src/renderer/terminalComposeSelection.ts`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{Document, Element, Event, HtmlTextAreaElement, Node};

use crate::input::compose_selection::{
    ComposeEffects, ComposeSelection, PaneInputs, SelectionChangeFacts,
};
use crate::input::pane_selection::PaneSelection;
use crate::input::selection::SelectionGuard;

mod textarea;

use textarea::{queue_microtask, schedule};

/// What the composer supplies, read live on every event.
pub struct ComposeSelectionOptions {
    /// Whether this composer is the active one for its pane.
    pub active: Box<dyn Fn() -> bool>,
    /// The composer's dock, whose focus keeps a collapse from releasing.
    pub dock: Box<dyn Fn() -> Option<Element>>,
    /// Runs once a mounted input has its caret placed after DOM insertion.
    pub after_input_mount: Box<dyn Fn()>,
}

impl std::fmt::Debug for ComposeSelectionOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ComposeSelectionOptions")
            .finish_non_exhaustive()
    }
}

type Listener = Closure<dyn FnMut(Event)>;

struct MountedInput {
    id: u32,
    element: HtmlTextAreaElement,
    listeners: Vec<(&'static str, Listener)>,
}

struct ComposeShared {
    pane: Rc<RefCell<PaneSelection>>,
    compose: RefCell<ComposeSelection>,
    input: RefCell<Option<MountedInput>>,
    next_input_id: Cell<u32>,
    options: ComposeSelectionOptions,
}

/// One composer's handoff with its pane's terminal selection.
pub struct TerminalComposeSelection {
    shared: Rc<ComposeShared>,
    selection_listener: Option<Listener>,
}

impl std::fmt::Debug for TerminalComposeSelection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalComposeSelection")
            .finish_non_exhaustive()
    }
}

impl TerminalComposeSelection {
    /// Start watching the document's selection for `pane`.
    pub fn new(pane: Rc<RefCell<PaneSelection>>, options: ComposeSelectionOptions) -> Self {
        let shared = Rc::new(ComposeShared {
            pane,
            compose: RefCell::new(ComposeSelection::new()),
            input: RefCell::new(None),
            next_input_id: Cell::new(1),
            options,
        });
        let owner = Rc::clone(&shared);
        let listener: Listener = Closure::new(move |_event: Event| owner.on_selection_change());
        if let Some(document) = shared.document() {
            let _ = document.add_event_listener_with_callback(
                "selectionchange",
                listener.as_ref().unchecked_ref(),
            );
        }
        Self {
            shared,
            selection_listener: Some(listener),
        }
    }

    /// Retain the pane's current selection.
    pub fn capture(&self) {
        self.shared.transition(ComposeSelection::capture);
    }

    /// Drop the retained range without touching the document's selection.
    pub fn release(&self) {
        self.shared.release();
    }

    /// Restore the retained range; false when it is gone.
    pub fn restore(&self) -> bool {
        self.shared.restore()
    }

    /// Restore after the browser's layout, as one versioned transaction.
    pub fn restore_after_layout(&self) {
        self.shared.restore_after_layout();
    }

    /// Whether a terminal range is retained.
    pub fn has_guard(&self) -> bool {
        self.shared
            .compose
            .try_borrow()
            .is_ok_and(|compose| compose.has_guard())
    }

    /// Whether an IME composition is in flight in the composer.
    pub fn is_composing(&self) -> bool {
        self.shared
            .compose
            .try_borrow()
            .is_ok_and(|compose| compose.is_composing())
    }

    /// The composer owns its caret now.
    pub fn mark_composer_selection_active(&self) {
        if let Ok(mut compose) = self.shared.compose.try_borrow_mut() {
            compose.mark_composer_selection_active();
        }
    }

    /// Remember the textarea's current selection.
    pub fn remember_composer_selection(&self) {
        self.shared.remember_composer_selection();
    }

    /// Remember a caret at `position` in the composer's text.
    pub fn remember_caret_at(&self, position: u32) {
        if let Ok(mut compose) = self.shared.compose.try_borrow_mut() {
            compose.remember_caret_at(position);
        }
    }

    /// A programmatic write has no pointerdown to capture the range first, so
    /// capture (when nothing is retained) and suspend together.
    pub fn prepare_programmatic_write(&self) {
        if !self.has_guard() {
            self.shared.transition(ComposeSelection::capture);
        }
        self.shared
            .transition(ComposeSelection::prepare_programmatic_write);
    }

    /// Attach the edit listeners to the composer's textarea, replacing any
    /// earlier one, and place its caret at the end once it is in the DOM.
    pub fn mount_input(&self, element: &HtmlTextAreaElement) {
        self.shared.unmount_input();
        let id = self.shared.next_input_id.get();
        self.shared.next_input_id.set(id.wrapping_add(1));
        let mut listeners = Vec::new();
        for kind in [
            "keydown",
            "beforeinput",
            "keyup",
            "compositionstart",
            "compositionend",
        ] {
            let owner = Rc::clone(&self.shared);
            let listener: Listener =
                Closure::new(move |_event: Event| owner.on_input_event(kind, id));
            let _ = element.add_event_listener_with_callback_and_bool(
                kind,
                listener.as_ref().unchecked_ref(),
                true,
            );
            listeners.push((kind, listener));
        }
        if let Ok(mut input) = self.shared.input.try_borrow_mut() {
            *input = Some(MountedInput {
                id,
                element: element.clone(),
                listeners,
            });
        }
        let owner = Rc::clone(&self.shared);
        let element = element.clone();
        schedule(move || {
            if !element.is_connected() {
                return;
            }
            let end = element.value().encode_utf16().count() as u32;
            let _ = element.set_selection_range(end, end);
            if let Ok(mut compose) = owner.compose.try_borrow_mut() {
                compose.remember_caret_at(end);
            }
            (owner.options.after_input_mount)();
        });
    }

    /// Detach every listener and drop the retained range.
    pub fn dispose(&mut self) {
        if let (Some(listener), Some(document)) =
            (self.selection_listener.take(), self.shared.document())
        {
            let _ = document.remove_event_listener_with_callback(
                "selectionchange",
                listener.as_ref().unchecked_ref(),
            );
        }
        self.shared.unmount_input();
        self.shared.release();
    }
}

impl Drop for TerminalComposeSelection {
    fn drop(&mut self) {
        self.dispose();
    }
}

impl ComposeShared {
    fn document(&self) -> Option<Document> {
        self.pane
            .try_borrow()
            .ok()
            .map(|pane| pane.document().clone())
    }

    /// Run one pure transition against a fresh read, then apply its effects.
    fn transition(
        self: &Rc<Self>,
        run: impl FnOnce(&mut ComposeSelection, &mut SelectionGuard, PaneInputs<'_>) -> ComposeEffects,
    ) -> ComposeEffects {
        let (Ok(mut pane), Ok(mut compose)) =
            (self.pane.try_borrow_mut(), self.compose.try_borrow_mut())
        else {
            return ComposeEffects::default();
        };
        let read = pane.read();
        let was_active = compose.composer_selection_active();
        let effects = run(&mut compose, pane.guard_mut(), read.inputs());
        let adopted = !was_active && compose.composer_selection_active();
        drop(compose);
        pane.apply(&effects);
        drop(pane);
        self.follow_up(effects, adopted);
        effects
    }

    fn follow_up(self: &Rc<Self>, effects: ComposeEffects, adopted: bool) {
        if let Some(selection) = effects.set_composer_selection {
            if !self.write_composer_selection(selection) {
                // A disabled or disconnected replacement cannot become an
                // editing surface.
                self.release();
            }
        } else if adopted {
            self.remember_composer_selection();
        }
        if let Some(version) = effects.schedule_layout_restore {
            self.schedule_layout_restore(version);
        }
        if let Some(input) = effects.schedule_keyup_restore {
            let owner = Rc::clone(self);
            schedule(move || owner.run_keyup_restore(input));
        }
        if effects.restore_next_microtask {
            let owner = Rc::clone(self);
            queue_microtask(move || {
                owner.restore();
            });
        }
    }

    fn release(self: &Rc<Self>) {
        self.transition(|compose, guard, _| compose.release(guard));
    }

    fn restore(self: &Rc<Self>) -> bool {
        let active = match self.compose.try_borrow() {
            Ok(compose) if compose.has_guard() => compose.composer_selection_active(),
            _ => return false,
        };
        if active {
            self.remember_composer_selection();
        }
        let (Ok(mut pane), Ok(mut compose)) =
            (self.pane.try_borrow_mut(), self.compose.try_borrow_mut())
        else {
            return false;
        };
        let read = pane.write_restore();
        let effects = compose.restore(pane.guard_mut(), read.inputs());
        drop(compose);
        pane.apply(&effects);
        effects.restore
    }

    fn restore_after_layout(self: &Rc<Self>) {
        let input = self.mounted_input_id();
        let pending = match self.compose.try_borrow_mut() {
            Ok(mut compose) => {
                compose.restore_after_layout(input);
                compose.pending_layout_version()
            }
            Err(_) => return,
        };
        if let Some((version, _)) = pending {
            self.schedule_layout_restore(version);
        }
    }

    fn schedule_layout_restore(self: &Rc<Self>, version: u64) {
        let Some((_, epoch)) = self
            .compose
            .try_borrow()
            .ok()
            .and_then(|c| c.pending_layout_version())
        else {
            return;
        };
        let owner = Rc::clone(self);
        schedule(move || {
            let current = match owner.compose.try_borrow_mut() {
                Ok(mut compose) if compose.layout_restore_is_current(version, epoch) => {
                    compose.finish_layout_restore();
                    true
                }
                _ => false,
            };
            if current {
                owner.restore();
            }
        });
    }

    fn run_keyup_restore(self: &Rc<Self>, input: u32) {
        let current = (self.options.active)()
            && self.focused_input_id() == Some(input)
            && self
                .compose
                .try_borrow()
                .is_ok_and(|compose| compose.keyup_restore_is_current(input));
        if current && self.restore() {
            if let Ok(mut compose) = self.compose.try_borrow_mut() {
                compose.finish_keyup_restore(input);
            }
        }
    }

    fn on_selection_change(self: &Rc<Self>) {
        if !(self.options.active)() {
            return;
        }
        let focus_in_dock = match ((self.options.dock)(), self.document()) {
            (Some(dock), Some(document)) => document.active_element().is_some_and(|active| {
                let dock: &Node = dock.as_ref();
                dock.contains(Some(active.as_ref()))
            }),
            _ => false,
        };
        let facts = SelectionChangeFacts {
            focus_in_dock,
            focused_input: self.focused_input_id(),
        };
        self.transition(|compose, guard, inputs| {
            compose.on_document_selection_change(guard, inputs, facts)
        });
    }

    fn on_input_event(self: &Rc<Self>, kind: &str, input: u32) {
        match kind {
            "keydown" | "beforeinput" => {
                self.transition(ComposeSelection::on_key_or_before_input);
            }
            "compositionstart" => {
                self.transition(ComposeSelection::on_composition_start);
            }
            "keyup" => {
                self.transition(|compose, _, _| compose.on_key_up(input));
            }
            "compositionend" => {
                self.transition(|compose, _, _| compose.on_composition_end());
            }
            _ => {}
        }
    }
}
