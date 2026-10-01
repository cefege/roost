//! The picker's browser questions: focus an element by id, where a floating
//! menu anchors, whether a key press happened inside a region, and how many
//! columns the entry grid is painting. Called by `browse::picker` and
//! `browse::picker::parts`; inert off the browser, because a native build paints
//! nothing.
//!
//! The column count is read from the LIVE grid rather than from a width
//! breakpoint because CSS owns it: `auto-fill` decides how many entries fit, and
//! a second copy of that arithmetic here would be a second answer to "one row
//! down".

/// The picker's own surface element id.
pub const SURFACE_ID: &str = "browse-surface";
/// The entry region's element id.
pub const RESULTS_ID: &str = "browse-results";
/// The filter field's element id, so the toggle can focus it.
pub const FILTER_ID: &str = "browse-filter-input";
/// The new-folder name field's element id, so opening the dialog can focus it.
pub const NEW_FOLDER_ID: &str = "browse-new-folder-input";

/// Focus the element with `id`, if it is in the document.
#[cfg(target_arch = "wasm32")]
pub fn focus_by_id(id: &str) {
    use wasm_bindgen::JsCast as _;

    let Some(element) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .and_then(|element| element.dyn_into::<web_sys::HtmlElement>().ok())
    else {
        return;
    };
    let _ = element.focus();
}

/// Focus the element with `id`, if it is in the document.
#[cfg(not(target_arch = "wasm32"))]
pub fn focus_by_id(_id: &str) {}

/// A trigger's right and bottom edges and the viewport's width, so a menu can
/// anchor to it. `None` when the trigger is not in the document, which is the
/// honest answer before the first paint.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn trigger_anchor(id: &str) -> Option<(f64, f64, f64)> {
    let bounds = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .map(|element| element.get_bounding_client_rect())?;
    let viewport_width = web_sys::window()
        .and_then(|window| window.inner_width().ok())
        .and_then(|width| width.as_f64())
        .unwrap_or(bounds.right());
    Some((bounds.right(), bounds.bottom(), viewport_width))
}

/// A trigger's right and bottom edges and the viewport's width, so a menu can
/// anchor to it.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn trigger_anchor(_id: &str) -> Option<(f64, f64, f64)> {
    None
}

/// A trigger's top-left corner, so a menu can hang below and left-align to it.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn trigger_corner(id: &str) -> Option<(f64, f64)> {
    let bounds = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.get_element_by_id(id))
        .map(|element| element.get_bounding_client_rect())?;
    Some((bounds.left(), bounds.bottom()))
}

/// A trigger's top-left corner, so a menu can hang below and left-align to it.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn trigger_corner(_id: &str) -> Option<(f64, f64)> {
    None
}

/// Whether the element a key press landed on is `id` or lives inside it.
///
/// Browser-only, and with no host twin on purpose: the picker installs this
/// question from inside its `window` keydown listener, and a build with no
/// window never gets the event that would answer it. The sibling functions
/// above keep host twins because the page CALLS them while rendering.
#[cfg(target_arch = "wasm32")]
#[must_use]
pub fn press_path_contains(id: &str, event: &web_sys::KeyboardEvent) -> bool {
    use wasm_bindgen::JsCast as _;

    let Some(mut node) = event
        .target()
        .and_then(|target| target.dyn_into::<web_sys::Node>().ok())
    else {
        return false;
    };
    loop {
        if node
            .dyn_ref::<web_sys::Element>()
            .is_some_and(|element| element.id() == id)
        {
            return true;
        }
        match node.parent_node() {
            Some(parent) => node = parent,
            None => return false,
        }
    }
}

/// How many columns the entry grid is painting, at least one.
#[must_use]
pub fn grid_columns() -> usize {
    #[cfg(target_arch = "wasm32")]
    {
        let selector = format!(".{}", crate::components::md::list::GRID_CLASS);
        let template = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(&selector).ok())
            .flatten()
            .and_then(|grid| {
                web_sys::window()
                    .and_then(|window| window.get_computed_style(&grid).ok())
                    .flatten()
                    .and_then(|style| style.get_property_value("grid-template-columns").ok())
            })
            .unwrap_or_default();
        return template.split_whitespace().count().max(1);
    }
    #[cfg(not(target_arch = "wasm32"))]
    1
}

/// Scroll the entry at `index` into view, so the keyboard cursor stays visible.
#[cfg(target_arch = "wasm32")]
pub fn scroll_entry_into_view(index: i64) {
    use wasm_bindgen::JsCast as _;

    if index < 0 {
        return;
    }
    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return;
    };
    let Ok(rows) = document.query_selector_all("[data-testid='browse-row']") else {
        return;
    };
    let Some(row) = rows.item(u32::try_from(index).unwrap_or_default()) else {
        return;
    };
    if let Some(row) = row.dyn_ref::<web_sys::Element>() {
        row.scroll_into_view();
    }
}

/// Scroll the entry at `index` into view, so the keyboard cursor stays visible.
#[cfg(not(target_arch = "wasm32"))]
pub fn scroll_entry_into_view(_index: i64) {}
