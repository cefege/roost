//! Document visibility: whether this browser tab is foregrounded.
//!
//! Every consumer reads visibility through `page_visible()`, so a native build
//! answers the same question the browser does instead of each caller deciding
//! what "no document" means.

/// Whether the browser tab is foregrounded.
#[cfg(target_arch = "wasm32")]
pub fn page_visible() -> bool {
    web_sys::window()
        .and_then(|window| window.document())
        .is_none_or(|document| !document.hidden())
}

/// No document to read, so a native build is visible. The same answer the
/// terminal's DOM helpers give for every other browser capability they cannot
/// have, and the one that keeps a native test exercising the same components a
/// browser runs rather than a set that exists only natively.
#[cfg(not(target_arch = "wasm32"))]
pub fn page_visible() -> bool {
    true
}
