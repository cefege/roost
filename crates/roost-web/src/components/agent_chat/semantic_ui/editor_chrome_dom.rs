//! Measures visible native editor rows for the source-derived frame edges.
//! The observer owns no text, focus, selection or input listeners.
//! Unmount disconnects its one resize subscription.
use dioxus::prelude::*;
use std::rc::Rc;
use wasm_bindgen::{JsCast, closure::Closure};
use web_sys::{HtmlElement, ResizeObserver};

pub(super) struct ChromeGeometry {
    observer: ResizeObserver,
    _resized: Closure<dyn FnMut(js_sys::Array, ResizeObserver)>,
}

impl ChromeGeometry {
    pub(super) fn mount(element_id: &str, rows: Signal<usize>) -> Option<Self> {
        let input = web_sys::window()?
            .document()?
            .get_element_by_id(element_id)?
            .dyn_into::<HtmlElement>()
            .ok()?;
        let input = Rc::new(input);
        let resized_input = input.clone();
        let resized = Closure::<dyn FnMut(js_sys::Array, ResizeObserver)>::new(move |_, _| {
            measure(&resized_input, rows);
        });
        let observer = ResizeObserver::new(resized.as_ref().unchecked_ref()).ok()?;
        observer.observe(&input);
        measure(&input, rows);
        Some(Self {
            observer,
            _resized: resized,
        })
    }
}

impl Drop for ChromeGeometry {
    fn drop(&mut self) {
        self.observer.disconnect();
    }
}

fn measure(input: &HtmlElement, mut rows: Signal<usize>) {
    let Some(computed) =
        web_sys::window().and_then(|window| window.get_computed_style(input).ok().flatten())
    else {
        return;
    };
    let pixels = |property: &str| {
        computed
            .get_property_value(property)
            .ok()
            .and_then(|value| value.trim_end_matches("px").parse::<f64>().ok())
    };
    let Some(line_height) =
        pixels("line-height").filter(|height| *height > 0.0 && height.is_finite())
    else {
        return;
    };
    let content_height = f64::from(input.client_height())
        - pixels("padding-top").unwrap_or_default()
        - pixels("padding-bottom").unwrap_or_default();
    let count = (content_height / line_height).round().clamp(1.0, 128.0) as usize;
    if *rows.peek() != count {
        rows.set(count);
    }
}
