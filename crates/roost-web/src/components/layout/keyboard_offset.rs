//! The soft keyboard's cover, published as `--kb-offset` on `<html>`.
//!
//! The variable is consumed by the shell's editor shift
//! (`shell_style`), the chat dock (`voice-input.css`), the machine menu sheet
//! and the notification dock lift, but nothing wrote it: the keyboard would
//! translate the terminal by zero pixels on iOS and Android. A
//! `visualViewport` `resize`/`scroll` listener measures the covered band —
//! the layout viewport height the page laid out against, minus what the
//! visual viewport still shows above the keyboard — and clamps it at zero.
//!
//! Consumers decide translate versus resize (`data-keyboard-shift` and the
//! `keyboard_resize` preference); this writer only publishes the measurement,
//! so both readouts stay correct under either mode.

/// The covered band one soft keyboard occupies, in CSS pixels.
///
/// `layout_height` is the height the page was laid out against (the layout
/// viewport), `viewport_height` and `viewport_offset_top` are the visual
/// viewport's `height` and `offsetTop`. Scroll and pinch can push the visual
/// viewport above the layout box, so the band clamps at zero rather than
/// going negative.
#[must_use]
pub fn keyboard_cover_px(
    layout_height: f64,
    viewport_height: f64,
    viewport_offset_top: f64,
) -> u32 {
    let covered = layout_height - viewport_height - viewport_offset_top;
    covered.max(0.0).round() as u32
}

/// The `--kb-offset` value for a covered band.
#[must_use]
pub fn keyboard_offset_value(covered_px: u32) -> String {
    format!("{covered_px}px")
}

#[cfg(target_arch = "wasm32")]
mod mount {
    use super::{keyboard_cover_px, keyboard_offset_value};
    use dioxus::prelude::*;
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    struct ViewportListeners {
        resize: Closure<dyn FnMut()>,
        scroll: Closure<dyn FnMut()>,
    }

    fn document_root() -> Option<web_sys::HtmlElement> {
        web_sys::window()?
            .document()?
            .document_element()?
            .dyn_into()
            .ok()
    }

    /// The layout viewport height the page laid out against.
    fn layout_height() -> f64 {
        document_root()
            .map(|root| f64::from(root.client_height()))
            .unwrap_or(0.0)
    }

    /// Measure and publish once, on the document root.
    fn publish() {
        let Some(root) = document_root() else {
            return;
        };
        let Some(viewport) = web_sys::window().and_then(|window| window.visual_viewport()) else {
            return;
        };
        let covered = keyboard_cover_px(layout_height(), viewport.height(), viewport.offset_top());
        let _ = root
            .style()
            .set_property("--kb-offset", &keyboard_offset_value(covered));
        tracing::debug!(target: "shell", covered, "published the soft keyboard cover");
    }

    /// Keep `--kb-offset` on `<html>` in step with the visual viewport;
    /// removed on unmount. Native builds have no viewport and skip it.
    pub fn use_keyboard_offset() {
        let listeners = use_hook(|| {
            let resize = Closure::<dyn FnMut()>::new(publish);
            let scroll = Closure::<dyn FnMut()>::new(publish);
            if let Some(viewport) = web_sys::window().and_then(|window| window.visual_viewport()) {
                let _ = viewport
                    .add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref());
                let _ = viewport
                    .add_event_listener_with_callback("scroll", scroll.as_ref().unchecked_ref());
            }
            publish();
            std::rc::Rc::new(ViewportListeners { resize, scroll })
        });
        use_drop(move || {
            if let Some(viewport) = web_sys::window().and_then(|window| window.visual_viewport()) {
                let _ = viewport.remove_event_listener_with_callback(
                    "resize",
                    listeners.resize.as_ref().unchecked_ref(),
                );
                let _ = viewport.remove_event_listener_with_callback(
                    "scroll",
                    listeners.scroll.as_ref().unchecked_ref(),
                );
            }
            if let Some(root) = document_root() {
                let _ = root.style().remove_property("--kb-offset");
            }
        });
    }
}

#[cfg(target_arch = "wasm32")]
pub use mount::use_keyboard_offset;

#[cfg(test)]
mod tests {
    use super::{keyboard_cover_px, keyboard_offset_value};

    #[test]
    fn an_open_keyboard_covers_the_gap_between_the_viewports() {
        assert_eq!(keyboard_cover_px(800.0, 500.0, 0.0), 300);
    }

    #[test]
    fn a_scrolled_viewport_counts_its_top_offset() {
        assert_eq!(keyboard_cover_px(800.0, 500.0, 60.0), 240);
    }

    #[test]
    fn a_viewport_above_the_layout_box_covers_nothing() {
        assert_eq!(keyboard_cover_px(800.0, 900.0, 0.0), 0);
        assert_eq!(keyboard_cover_px(800.0, 850.0, 20.0), 0);
    }

    #[test]
    fn a_closed_keyboard_covers_nothing() {
        assert_eq!(keyboard_cover_px(800.0, 800.0, 0.0), 0);
    }

    #[test]
    fn a_fractional_cover_rounds_to_whole_pixels() {
        assert_eq!(keyboard_cover_px(800.5, 500.0, 0.0), 301);
        assert_eq!(keyboard_cover_px(800.4, 500.0, 0.0), 300);
    }

    #[test]
    fn the_value_is_a_pixel_length_set_property_accepts() {
        assert_eq!(keyboard_offset_value(300), "300px");
        assert_eq!(keyboard_offset_value(0), "0px");
    }
}
