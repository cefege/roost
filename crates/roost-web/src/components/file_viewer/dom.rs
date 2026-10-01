//! The four browser questions the file viewer sheet asks: which line the
//! address bar's `#L<n>` names, the URL a copied line link is, where focus
//! lands when the machine is gone, and how far the body has to scroll to reach
//! the target line. Called by `file_viewer` and `file_viewer::body`; inert off
//! the browser, because a native build paints no document to ask.
//!
//! Every function here is a read or a scroll. Nothing in this module mutates
//! what the reader sees, which is why each one has an honest answer when the
//! document is not there.

/// The line the address bar's `#L<n>` fragment names, or 1 when it names none.
///
/// v2 `parseLineFromHash`. A fragment that is not `#L<digits>` is not a line
/// reference, and a fragment that names line 0 has no row to mark.
#[must_use]
pub fn target_line_from_hash() -> usize {
    parse_line_from_hash(&location_hash())
}

/// The line a `#L<n>` fragment names, or 1. Pure, so the fragment grammar is
/// testable without a document.
#[must_use]
pub fn parse_line_from_hash(hash: &str) -> usize {
    let Some(digits) = hash.strip_prefix("#L") else {
        return 1;
    };
    digits
        .parse::<usize>()
        .ok()
        .filter(|line| *line > 0)
        .unwrap_or(1)
}

/// The shareable URL of the sheet as it stands, with `line` as its fragment.
///
/// v2 `lineUrl`: the address the reader is already on, pointed at one line.
#[must_use]
pub fn line_link_url(path_and_query: &str, line: usize) -> String {
    format!("{path_and_query}#L{line}")
}

/// Put focus on the element with this id, so a reader who lands on a denial can
/// leave it with the keyboard alone.
pub fn focus_by_id(id: &str) {
    focus_element(id);
}

/// Scroll the row carrying `data-line="<line>"` to the middle of the body, the
/// line the address bar asked for. A row that is not in the document yet — the
/// read has not landed — is not an error; the caller scrolls again when it does.
pub fn scroll_line_into_view(line: usize) {
    scroll_line_into_view_next_frame(line);
}

/// Bring the denial's recovery action into the middle of the region it lives in
/// when it takes focus. At a short viewport the region scrolls, and a focused
/// control below its fold is a control the reader has been told about and
/// cannot see.
pub fn scroll_home_action_into_view() {
    scroll_into_view_center(HOME_ACTION_TEST_ID);
}

/// The recovery action's test id, which is also how it is found in the document.
const HOME_ACTION_TEST_ID: &str = "file-viewer-unavailable-home";

#[cfg(target_arch = "wasm32")]
fn location_hash() -> String {
    web_sys::window()
        .and_then(|window| window.location().hash().ok())
        .unwrap_or_default()
}

#[cfg(not(target_arch = "wasm32"))]
fn location_hash() -> String {
    String::new()
}

#[cfg(target_arch = "wasm32")]
fn focus_element(id: &str) {
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

#[cfg(not(target_arch = "wasm32"))]
fn focus_element(_id: &str) {}

#[cfg(target_arch = "wasm32")]
fn scroll_line_into_view_next_frame(line: usize) {
    use wasm_bindgen::JsCast as _;
    use wasm_bindgen::closure::Closure;

    let Some(window) = web_sys::window() else {
        return;
    };
    let selector = format!("[data-line=\"{line}\"]");
    let callback = Closure::once_into_js(move || {
        let Some(row) = web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.query_selector(&selector).ok().flatten())
        else {
            return;
        };
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_block(web_sys::ScrollLogicalPosition::Center);
        row.scroll_into_view_with_scroll_into_view_options(&options);
    });
    let _ = window.request_animation_frame(callback.unchecked_ref());
}

#[cfg(not(target_arch = "wasm32"))]
fn scroll_line_into_view_next_frame(_line: usize) {}

#[cfg(target_arch = "wasm32")]
fn scroll_into_view_center(test_id: &str) {
    let selector = format!("[data-testid=\"{test_id}\"]");
    let Some(target) = web_sys::window()
        .and_then(|window| window.document())
        .and_then(|document| document.query_selector(&selector).ok().flatten())
    else {
        return;
    };
    let options = web_sys::ScrollIntoViewOptions::new();
    options.set_block(web_sys::ScrollLogicalPosition::Center);
    target.scroll_into_view_with_scroll_into_view_options(&options);
}

#[cfg(not(target_arch = "wasm32"))]
fn scroll_into_view_center(_test_id: &str) {}

#[cfg(test)]
mod tests {
    use super::parse_line_from_hash;

    #[test]
    fn a_line_fragment_names_that_line() {
        assert_eq!(parse_line_from_hash("#L9"), 9);
        assert_eq!(parse_line_from_hash("#L128"), 128);
    }

    #[test]
    fn a_fragment_that_names_no_line_falls_back_to_the_first() {
        for hash in ["", "#L", "#Lx", "#L0", "#line9", "#9", "L9", "#L9x"] {
            assert_eq!(
                parse_line_from_hash(hash),
                1,
                "{hash} was not the first line"
            );
        }
    }
}
