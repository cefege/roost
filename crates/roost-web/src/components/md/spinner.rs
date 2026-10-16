//! The braille loader glyph OMP draws beside a running turn. Decorative: it is
//! hidden from assistive technology, and the row's label carries the state.
//! Callers pick the frame; this primitive owns the markup and its colour.

use dioxus::prelude::*;

use super::stylesheet::{ICON_STYLESHEET_HREF, use_md_stylesheet};

/// One frame of the loader. Its colour rule lives in `icon.css`, so the primitive
/// attaches that sheet the way `Icon` does.
#[component]
pub fn Spinner(frame: char) -> Element {
    use_md_stylesheet(ICON_STYLESHEET_HREF);
    rsx! {
        span { class: "md-spinner", "aria-hidden": "true", "{frame}" }
    }
}
