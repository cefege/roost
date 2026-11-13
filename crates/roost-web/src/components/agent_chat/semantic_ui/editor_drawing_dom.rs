//! Browser-owned mirror geometry, selection observation and composition visibility.
//! The editor drawing owns this listener/observer bundle through unmount.
//! Text stays in the native control; only its drawing is replaced outside IME.

use dioxus::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{Event, EventTarget, HtmlElement, ResizeObserver};

struct DrawingState {
    input: HtmlElement,
    clip: HtmlElement,
    mirror: HtmlElement,
    text: String,
    enabled: bool,
    composing: bool,
    caret: Signal<usize>,
    native_cursor: usize,
    fill: String,
}

type DomListener = (EventTarget, &'static str, Closure<dyn FnMut(Event)>);

pub(super) struct DrawingDom {
    state: Rc<RefCell<DrawingState>>,
    listeners: Vec<DomListener>,
    observer: ResizeObserver,
    _resized: Closure<dyn FnMut(js_sys::Array, ResizeObserver)>,
}

impl DrawingDom {
    pub(super) fn mount(id: &str, caret: Signal<usize>) -> Option<Self> {
        let document = web_sys::window()?.document()?;
        let input = document
            .get_element_by_id(id)?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let clip = document
            .get_element_by_id(&format!("{id}-drawing"))?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let mirror = document
            .get_element_by_id(&format!("{id}-mirror"))?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let style = input.style();
        let state = Rc::new(RefCell::new(DrawingState {
            fill: style
                .get_property_value("-webkit-text-fill-color")
                .unwrap_or_default(),
            native_cursor: *caret.peek(),
            input,
            clip,
            mirror,
            text: String::new(),
            enabled: false,
            composing: false,
            caret,
        }));
        let resize_state = state.clone();
        let resized = Closure::<dyn FnMut(js_sys::Array, ResizeObserver)>::new(move |_, _| {
            sync(&resize_state)
        });
        let observer = ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
        let mut listeners = Vec::new();
        for event_name in [
            "scroll",
            "input",
            "select",
            "keyup",
            "click",
            "compositionstart",
            "compositionend",
        ] {
            let shared = state.clone();
            let callback = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
                {
                    let mut state = shared.borrow_mut();
                    match event.type_().as_str() {
                        "compositionstart" => state.composing = true,
                        "compositionend" => state.composing = false,
                        _ => {}
                    }
                }
                sync(&shared);
            });
            let target: EventTarget = state.borrow().input.clone().into();
            let _ = target
                .add_event_listener_with_callback(event_name, callback.as_ref().unchecked_ref());
            listeners.push((target, event_name, callback));
        }
        let selection_state = state.clone();
        let selected = Closure::<dyn FnMut(Event)>::new(move |_| sync(&selection_state));
        let target: EventTarget = document.into();
        let _ = target
            .add_event_listener_with_callback("selectionchange", selected.as_ref().unchecked_ref());
        listeners.push((target, "selectionchange", selected));
        observer.observe(&state.borrow().input);
        Some(Self {
            state,
            listeners,
            observer,
            _resized: resized,
        })
    }

    pub(super) fn update(&mut self, text: String, enabled: bool, cursor: usize) {
        {
            let mut state = self.state.borrow_mut();
            state.text = text;
            state.enabled = enabled;
            state.native_cursor = cursor;
        }
        sync(&self.state);
    }
}

impl Drop for DrawingDom {
    fn drop(&mut self) {
        self.observer.disconnect();
        for (target, event, callback) in &self.listeners {
            let _ = target
                .remove_event_listener_with_callback(event, callback.as_ref().unchecked_ref());
        }
        let state = self.state.borrow();
        restore_style(&state.input, "-webkit-text-fill-color", &state.fill);
    }
}

fn restore_style(input: &HtmlElement, property: &str, saved: &str) {
    if saved.is_empty() {
        let _ = input.style().remove_property(property);
    } else {
        let _ = input.style().set_property(property, saved);
    }
}

fn sync(shared: &Rc<RefCell<DrawingState>>) {
    let state = shared.borrow();
    let id = state.input.id();
    let current = super::super::super::text_edit::current_value(&id);
    let visible = state.enabled
        && !state.composing
        && !state.input.has_attribute("data-omp-composing")
        && current.as_deref() == Some(state.text.as_str());
    let _ = state
        .clip
        .style()
        .set_property("visibility", if visible { "visible" } else { "hidden" });
    if !visible {
        restore_style(&state.input, "-webkit-text-fill-color", &state.fill);
        return;
    }
    let Some(computed) =
        web_sys::window().and_then(|window| window.get_computed_style(&state.input).ok().flatten())
    else {
        return;
    };
    let mirror = state.mirror.style();
    for property in [
        "font-family",
        "font-size",
        "font-weight",
        "font-style",
        "font-variant",
        "line-height",
        "letter-spacing",
        "word-spacing",
        "text-transform",
        "text-indent",
        "text-align",
        "tab-size",
        "direction",
        "padding-top",
        "padding-right",
        "padding-bottom",
        "padding-left",
        "overflow-wrap",
        "word-break",
        "text-rendering",
    ] {
        if let Ok(value) = computed.get_property_value(property) {
            let _ = mirror.set_property(property, &value);
        }
    }
    let _ = mirror.set_property("box-sizing", "border-box");
    let _ = mirror.set_property("width", &format!("{}px", state.input.client_width()));
    let single_line = state.input.dyn_ref::<web_sys::HtmlInputElement>().is_some();
    if single_line {
        let pixels = |property: &str| {
            computed
                .get_property_value(property)
                .ok()
                .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
        };
        if let (Some(top), Some(bottom), Some(line)) = (
            pixels("padding-top"),
            pixels("padding-bottom"),
            pixels("line-height"),
        ) {
            let extra =
                (f64::from(state.input.client_height()) - top - bottom - line).max(0.0) / 2.0;
            let _ = mirror.set_property("padding-top", &format!("{}px", top + extra));
        }
    }
    let _ = mirror.set_property("white-space", if single_line { "pre" } else { "pre-wrap" });
    let _ = mirror.set_property(
        "transform",
        &format!(
            "translate({}px, {}px)",
            -state.input.scroll_left(),
            -state.input.scroll_top()
        ),
    );
    let _ = state
        .clip
        .style()
        .set_property("left", &format!("{}px", state.input.client_left()));
    let _ = state
        .clip
        .style()
        .set_property("top", &format!("{}px", state.input.client_top()));
    let _ = state
        .clip
        .style()
        .set_property("width", &format!("{}px", state.input.client_width()));
    let _ = state
        .clip
        .style()
        .set_property("height", &format!("{}px", state.input.client_height()));
    let _ = state
        .input
        .style()
        .set_property("-webkit-text-fill-color", "transparent");
    let active = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.active_element())
        .is_some_and(|active| active.id() == id);
    let next = if active {
        super::super::super::text_edit::current_selection(&id)
    } else {
        Some(state.native_cursor)
    };
    let mut caret = state.caret;
    drop(state);
    if let Some(next) = next
        && *caret.peek() != next
    {
        caret.set(next);
    }
}
