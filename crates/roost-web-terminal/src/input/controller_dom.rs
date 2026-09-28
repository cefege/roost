//! The DOM half of the pane input controller: one off-screen textarea per
//! mounted pane, its seven listeners, and the focus dance. Every decision is
//! `controller::InputControllerState`'s; this file reads events into it and
//! applies what it returns. The terminal pane constructs one per mount and
//! calls `force_focus` from its container's `mousedown`.
//! Ports v2's `apps/web/src/renderer/terminalInputController.ts`.

use std::cell::RefCell;
use std::rc::Rc;

use wasm_bindgen::JsCast;
use wasm_bindgen::closure::Closure;
use web_sys::{
    ClipboardEvent, CompositionEvent, Document, Element, Event, FocusEvent, FocusEventInit,
    FocusOptions, HtmlTextAreaElement, InputEvent, KeyboardEvent, Node,
};

use crate::cell_renderer_dom::DomSetupError;
use crate::input::chord::Modifiers;
use crate::input::controller::{
    FocusSurface, InputControllerState, KeyDownAction, TerminalKeyEvent, TextareaCommit,
    force_focus,
};

/// What the pane hands the controller. The mode reads are called per event,
/// so a worker-reported DECCKM or DECSET 1004 change applies to the next key.
pub struct TerminalInputOptions {
    /// DECCKM as the worker reports it.
    pub cursor_keys_application: Box<dyn Fn() -> bool>,
    /// DECSET 1004 as the worker reports it: real focus and blur become PTY
    /// reports while the application asks for them.
    pub focus_events_enabled: Box<dyn Fn() -> bool>,
    /// Bytes for the session's input lane.
    pub on_data: Box<dyn Fn(&str)>,
    /// Clipboard admission belongs to the pane, so multiline confirmation,
    /// attachment extraction, bracketed framing and queue limits share one path.
    pub on_paste: Box<dyn Fn(&str, &ClipboardEvent)>,
    /// The textarea's accessible name; "Terminal input" when absent.
    pub aria_label: Option<String>,
    /// TV mode keeps directional navigation off the off-screen textarea.
    pub tv_mode_active: bool,
}

impl std::fmt::Debug for TerminalInputOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalInputOptions")
            .field("aria_label", &self.aria_label)
            .field("tv_mode_active", &self.tv_mode_active)
            .finish_non_exhaustive()
    }
}

struct ControllerShared {
    state: RefCell<InputControllerState>,
    root: Element,
    textarea: HtmlTextAreaElement,
    doc: Document,
    options: TerminalInputOptions,
}

type Listener = Closure<dyn FnMut(Event)>;

/// One pane's textarea and the listeners on it.
pub struct TerminalInputController {
    shared: Rc<ControllerShared>,
    listeners: Vec<(&'static str, Listener)>,
}

impl std::fmt::Debug for TerminalInputController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalInputController")
            .field("listeners", &self.listeners.len())
            .finish_non_exhaustive()
    }
}

impl TerminalInputController {
    /// Create the pane's textarea inside `root` and start listening on it.
    pub fn new(root: &Element, options: TerminalInputOptions) -> Result<Self, DomSetupError> {
        let refused = |tag: &str| DomSetupError::RefusedTag {
            tag: tag.to_string(),
        };
        let doc = root.owner_document().ok_or_else(|| refused("document"))?;
        let textarea: HtmlTextAreaElement = doc
            .create_element("textarea")
            .map_err(|_| refused("textarea"))?
            .dyn_into()
            .map_err(|_| DomSetupError::NotHtmlElement {
                tag: "textarea".to_string(),
            })?;
        textarea.set_class_name("terminal-input");
        for (name, value) in [
            ("autocapitalize", "off"),
            ("autocomplete", "off"),
            ("autocorrect", "off"),
            ("spellcheck", "false"),
            ("enterkeyhint", "send"),
        ] {
            let _ = textarea.set_attribute(name, value);
        }
        let label = options.aria_label.as_deref().unwrap_or("Terminal input");
        let _ = textarea.set_attribute("aria-label", label);
        // The textarea sits off-screen; programmatic focus still works at -1,
        // but directional navigation must never land on it.
        textarea.set_tab_index(if options.tv_mode_active { -1 } else { 0 });
        let shared = Rc::new(ControllerShared {
            state: RefCell::new(InputControllerState::new()),
            root: root.clone(),
            textarea,
            doc,
            options,
        });
        let mut controller = Self {
            shared,
            listeners: Vec::new(),
        };
        controller.listen("keydown", on_key_down);
        controller.listen("paste", on_paste);
        controller.listen("compositionstart", on_composition_start);
        controller.listen("compositionend", on_composition_end);
        controller.listen("input", on_input);
        controller.listen("focus", |shared, _| on_focus_change(shared, true));
        controller.listen("blur", |shared, _| on_focus_change(shared, false));
        let textarea_node: &Node = controller.shared.textarea.as_ref();
        let _ = root.append_child(textarea_node);
        tracing::debug!(target: "input", "terminal input controller attached");
        Ok(controller)
    }

    /// The pane's textarea.
    pub fn textarea(&self) -> &HtmlTextAreaElement {
        &self.shared.textarea
    }

    /// Whether `target` is this pane's textarea.
    pub fn owns_target(&self, target: Option<&Element>) -> bool {
        let textarea: &Node = self.shared.textarea.as_ref();
        target.is_some_and(|target| {
            let target: &Node = target.as_ref();
            target.is_same_node(Some(textarea))
        })
    }

    /// Rename the textarea for assistive technology.
    pub fn set_accessible_label(&self, label: &str) {
        let _ = self.shared.textarea.set_attribute("aria-label", label);
    }

    /// The blur-first focus dance; see `controller::force_focus`.
    pub fn force_focus(&self) -> bool {
        force_focus(&self.shared.state, &TextareaSurface(&self.shared))
    }

    /// Encode one key through the physical-key path and write it. False when
    /// the key has no terminal encoding or the controller is destroyed.
    pub fn dispatch_keydown(&self, event: &TerminalKeyEvent<'_>) -> bool {
        let application = (self.shared.options.cursor_keys_application)();
        let Some(bytes) = self
            .shared
            .state
            .try_borrow()
            .ok()
            .and_then(|state| state.dispatch_keydown(event, application))
        else {
            return false;
        };
        (self.shared.options.on_data)(&bytes);
        true
    }

    /// Remove every listener and the textarea. Idempotent.
    pub fn destroy(&mut self) {
        match self.shared.state.try_borrow_mut() {
            Ok(mut state) if !state.is_destroyed() => state.destroy(),
            _ => return,
        }
        for (kind, listener) in self.listeners.drain(..) {
            let _ = self
                .shared
                .textarea
                .remove_event_listener_with_callback(kind, listener.as_ref().unchecked_ref());
        }
        let _ = self.shared.root.class_list().remove_1("focused");
        self.shared.textarea.remove();
        tracing::debug!(target: "input", "terminal input controller destroyed");
    }

    fn listen(&mut self, kind: &'static str, react: fn(&Rc<ControllerShared>, &Event)) {
        let shared = Rc::clone(&self.shared);
        let listener: Listener = Closure::new(move |event: Event| react(&shared, &event));
        if self
            .shared
            .textarea
            .add_event_listener_with_callback(kind, listener.as_ref().unchecked_ref())
            .is_err()
        {
            tracing::warn!(target: "input", kind, "the textarea refused a listener");
        }
        self.listeners.push((kind, listener));
    }
}

impl Drop for TerminalInputController {
    fn drop(&mut self) {
        self.destroy();
    }
}

fn on_key_down(shared: &Rc<ControllerShared>, event: &Event) {
    let Some(event) = event.dyn_ref::<KeyboardEvent>() else {
        return;
    };
    let key = event.key();
    let key_event = TerminalKeyEvent {
        key: &key,
        modifiers: Modifiers {
            shift: event.shift_key(),
            alt: event.alt_key(),
            ctrl: event.ctrl_key(),
            meta: event.meta_key(),
        },
        alt_graph: event.get_modifier_state("AltGraph"),
        is_composing: event.is_composing(),
    };
    let application = (shared.options.cursor_keys_application)();
    let Ok(state) = shared.state.try_borrow() else {
        return;
    };
    let action = state.key_down(&key_event, application, || selection_has_text(&shared.doc));
    drop(state);
    match action {
        KeyDownAction::Browser => {}
        // The platform paste is NOT prevented: its ClipboardEvent is admitted
        // once, through the paste listener, files and bracketed framing included.
        KeyDownAction::FocusForPaste => focus_without_scroll(&shared.textarea),
        KeyDownAction::SelectPane => {
            event.prevent_default();
            select_contents(&shared.doc, &shared.root);
        }
        KeyDownAction::Write(bytes) => {
            event.prevent_default();
            (shared.options.on_data)(&bytes);
        }
        KeyDownAction::Consume => event.prevent_default(),
    }
}

fn on_paste(shared: &Rc<ControllerShared>, event: &Event) {
    let Some(event) = event.dyn_ref::<ClipboardEvent>() else {
        return;
    };
    event.prevent_default();
    shared.textarea.set_value("");
    let text = event
        .clipboard_data()
        .and_then(|data| data.get_data("text").ok())
        .unwrap_or_default();
    (shared.options.on_paste)(&text, event);
}

fn on_composition_start(shared: &Rc<ControllerShared>, _event: &Event) {
    if let Ok(mut state) = shared.state.try_borrow_mut() {
        state.composition_start();
    }
    shared.textarea.set_value("");
}

fn on_composition_end(shared: &Rc<ControllerShared>, event: &Event) {
    let data = event.dyn_ref::<CompositionEvent>().and_then(CompositionEvent::data);
    let Ok(mut state) = shared.state.try_borrow_mut() else {
        return;
    };
    let pending = state.composition_end(data.as_deref(), &shared.textarea.value());
    drop(state);
    let owner = Rc::clone(shared);
    let settle = move || {
        let Ok(mut state) = owner.state.try_borrow_mut() else {
            return;
        };
        let commit = state.settle_composition(&pending, &owner.textarea.value());
        drop(state);
        write_commit(&owner, commit);
    };
    // The fallback waits one microtask, so the `input` that Chromium dispatches
    // in the same native turn cancels it and the text is written exactly once.
    match web_sys::window() {
        Some(window) => {
            let callback: js_sys::Function = Closure::once_into_js(settle).unchecked_into();
            window.queue_microtask(&callback);
        }
        None => settle(),
    }
}

fn on_input(shared: &Rc<ControllerShared>, event: &Event) {
    let (is_composing, data) = match event.dyn_ref::<InputEvent>() {
        Some(input) => (input.is_composing(), input.data()),
        None => (false, None),
    };
    let Ok(mut state) = shared.state.try_borrow_mut() else {
        return;
    };
    let commit = state.input(is_composing, &shared.textarea.value(), data.as_deref());
    drop(state);
    write_commit(shared, commit);
}

fn write_commit(shared: &ControllerShared, commit: Option<TextareaCommit>) {
    let Some(commit) = commit else {
        return;
    };
    shared.textarea.set_value("");
    if !commit.text.is_empty() {
        (shared.options.on_data)(&commit.text);
    }
}

/// Focus reporting rides the REAL textarea transition: the application asked
/// which surface owns the keyboard now, and only these two events answer it.
fn on_focus_change(shared: &Rc<ControllerShared>, focused: bool) {
    let classes = shared.root.class_list();
    let _ = if focused {
        classes.add_1("focused")
    } else {
        classes.remove_1("focused")
    };
    let enabled = (shared.options.focus_events_enabled)();
    let Ok(mut state) = shared.state.try_borrow_mut() else {
        return;
    };
    let report = state.focus_changed(focused, enabled);
    drop(state);
    if let Some(report) = report {
        (shared.options.on_data)(report);
    }
}

fn selection_has_text(doc: &Document) -> bool {
    doc.get_selection()
        .ok()
        .flatten()
        .is_some_and(|selection| selection.to_string().length() > 0)
}

fn select_contents(doc: &Document, root: &Element) {
    let (Ok(Some(selection)), Ok(range)) = (doc.get_selection(), doc.create_range()) else {
        return;
    };
    let root: &Node = root.as_ref();
    if range.select_node_contents(root).is_ok() {
        let _ = selection.remove_all_ranges();
        let _ = selection.add_range(&range);
    }
}

fn focus_without_scroll(textarea: &HtmlTextAreaElement) {
    let options = FocusOptions::new();
    options.set_prevent_scroll(true);
    let _ = textarea.focus_with_options(&options);
}

/// The live textarea as the focus dance drives it.
struct TextareaSurface<'a>(&'a ControllerShared);

impl FocusSurface for TextareaSurface<'_> {
    fn is_active(&self) -> bool {
        let textarea: &Node = self.0.textarea.as_ref();
        self.0.doc.active_element().is_some_and(|active| {
            let active: &Node = active.as_ref();
            active.is_same_node(Some(textarea))
        })
    }

    fn blur(&self) -> Result<(), ()> {
        self.0.textarea.blur().map_err(|_| ())
    }

    fn focus(&self) -> Result<(), ()> {
        let options = FocusOptions::new();
        options.set_prevent_scroll(true);
        self.0.textarea.focus_with_options(&options).map_err(|_| ())
    }

    fn dispatch_focus(&self) -> Result<(), ()> {
        let init = FocusEventInit::new();
        init.set_bubbles(true);
        let event = FocusEvent::new_with_focus_event_init_dict("focus", &init).map_err(|_| ())?;
        self.0.textarea.dispatch_event(&event).map(|_| ()).map_err(|_| ())
    }
}
