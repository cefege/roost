//! The layout switches every component reads: whether this is the compact
//! (phone) shell and whether the ten-foot TV layout is on. Phone-sized is
//! (`hooks/use_is_mobile.rs`, which uses Libero media observation and Roost's
//! short-side boundary); read by `AppShell`, `MainPane`, the deck, the sidebar,
//! and the overlays that become sheets on a phone.

use dioxus::prelude::*;

use super::shell_metrics::is_compact_shell;
use crate::components::hooks::use_is_mobile::use_is_mobile;

/// v2 `isCompact()`, TV-aware: whether this is the compact (phone) shell
/// (`shell_metrics::is_compact_shell`). Reading it subscribes the caller to
/// the window crossing the phone boundary.
pub fn use_is_compact() -> bool {
    let phone_sized = use_is_mobile();
    is_compact_shell(phone_sized(), use_tv_layout())
}

/// Whether the ten-foot layout is on: TV mode from the user agent, `?tv=`, or
/// Settings → Appearance. False before the document input installs the
/// modality.
pub fn use_tv_layout() -> bool {
    try_use_context::<Signal<crate::input_nav::NavModality>>()
        .is_some_and(|modality| modality.read().tv_mode_active())
}
