//! Keeps the focused preference row on screen, as the terminal list scrolls to its selection.
//! Mounted only inside the focused row; OMP stays the sole owner of which row is focused.
//! Scrolls the nearest scrollable ancestors by the minimum needed and never moves input focus.
use dioxus::prelude::*;

/// Scrolls the enclosing row into view whenever it becomes the focused row.
#[component]
pub(super) fn PrefsRevealFocus(row_id: String) -> Element {
    use_effect(use_reactive((&row_id,), move |(row_id,)| {
        reveal_row(&row_id)
    }));
    rsx! {}
}

#[cfg(any(target_arch = "wasm32", test))]
fn row_selector(row_id: &str) -> String {
    let mut escaped = String::with_capacity(row_id.len());
    for character in row_id.chars() {
        if matches!(character, '"' | '\\') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    format!("[data-omp-setting=\"{escaped}\"]")
}

#[cfg(target_arch = "wasm32")]
fn reveal_row(row_id: &str) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let selector = row_selector(row_id);
    // Layout of rows added in the same frame settles before the scroll is computed.
    let callback = Closure::once_into_js(move || {
        let Some(row) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(&selector).ok().flatten())
        else {
            return;
        };
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
        row.scroll_into_view_with_scroll_into_view_options(&options);
    });
    let _ = window.request_animation_frame(callback.unchecked_ref());
}

#[cfg(not(target_arch = "wasm32"))]
fn reveal_row(_row_id: &str) {}

#[cfg(test)]
mod tests {
    use super::row_selector;

    #[test]
    fn row_selector_escapes_quotes_and_backslashes() {
        assert_eq!(
            row_selector("theme.dark"),
            "[data-omp-setting=\"theme.dark\"]"
        );
        assert_eq!(
            row_selector("a\"b\\c"),
            "[data-omp-setting=\"a\\\"b\\\\c\"]"
        );
    }
}
