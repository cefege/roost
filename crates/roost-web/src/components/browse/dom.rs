//! Browser focus, event-scope and scrolling adapters for the folder picker.
//! Called by `browse::picker` and its controls; path and listing state live there.
//! DOM operations are inert in native builds.

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

/// Whether the element a key press landed on is `id` or lives inside it.
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
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
        row.scroll_into_view_with_scroll_into_view_options(&options);
    }
}

/// Scroll the entry at `index` into view, so the keyboard cursor stays visible.
#[cfg(not(target_arch = "wasm32"))]
pub fn scroll_entry_into_view(_index: i64) {}
