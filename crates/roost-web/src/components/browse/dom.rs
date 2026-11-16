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

/// Measure the live folder grid so vertical keys follow rows after a resize.
#[cfg(target_arch = "wasm32")]
pub fn folder_row_stride() -> i64 {
    use wasm_bindgen::JsCast as _;

    let Some(document) = web_sys::window().and_then(|window| window.document()) else {
        return 1;
    };
    let Ok(folders) = document.query_selector_all("[data-browse-folder]") else {
        return 1;
    };
    let mut first_top = None;
    let mut columns = 0;
    for index in 0..folders.length() {
        let Some(folder) = folders
            .item(index)
            .and_then(|node| node.dyn_into::<web_sys::Element>().ok())
        else {
            continue;
        };
        let top = folder.get_bounding_client_rect().top();
        let first_top = *first_top.get_or_insert(top);
        if (top - first_top).abs() >= 1.0 {
            break;
        }
        columns += 1;
    }
    columns.max(1)
}
