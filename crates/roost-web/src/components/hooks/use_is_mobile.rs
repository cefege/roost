//! rust-ui's `use_is_mobile`: whether the window is phone-sized. Roost keeps
//! its own boundary instead of the registry's 768px width: the SHORT side
//! under 600px, so a phone held in landscape stays mobile and a tablet in
//! portrait does not. Read by `layout::window_size::use_is_compact`.

use dioxus::prelude::*;

use super::use_media_query::use_media_query;

/// Either side under 600 CSS px. `.98` rather than `599px` keeps a fractional
/// viewport just under the boundary mobile, and the level-3 syntax (not
/// `width < 600px`) still parses in older Safari and TV browsers.
/// `assets/styles/theme-vars.css` gates the phone's chat-dock tokens on the
/// same query.
pub const MOBILE_QUERY: &str = "(max-width: 599.98px), (max-height: 599.98px)";

/// Whether the window is phone-sized, kept current as it resizes or rotates.
#[must_use]
pub fn use_is_mobile() -> ReadSignal<bool> {
    use_media_query(MOBILE_QUERY)
}
