//! Roost's compact boundary: the short side stays under 600 CSS pixels.
//! Landscape phones stay compact while portrait tablets do not.
//! Libero owns media observation; layout::window_size combines this with TV policy.

use dioxus::prelude::*;

use libero::hooks::use_media_query;

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
