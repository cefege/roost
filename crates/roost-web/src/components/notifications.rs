//! The bottom notification column and the two banners that own the corners
//! around it: toasts, undo snackbars, the transfer popup, the controller
//! legend, plus the connection and version notices. Mounted once by
//! `app::AuthorizedOverlays`; every child reads the client's toast, transfer and
//! pending-close state and performs the acknowledgement its own card rule names.
//! Ports `apps/web/src/components/notifications/`.

pub mod agent_notifications;
pub mod clipboard;
pub mod connection_banner;
pub mod notify_target;
pub mod pad_hint_bar;
pub mod store_write;
pub mod toast_card;
pub mod toast_stack;
pub mod transfer_outcome;
pub mod transfer_row;
pub mod transfers_panel;
pub mod undo_banner;
pub mod version_banner;

use dioxus::prelude::*;

use crate::components::md::stylesheet::use_md_stylesheet;

/// The dock's own sheet. Not in `Dioxus.toml`'s eager list, exactly as v2
/// reached its sheet through the lazy chunk that first rendered a notification.
pub const NOTIFICATION_DOCK_STYLESHEET: &str = "/components/notifications/NotificationDock.css";

/// The one overlay column that owns where every transient notification sits, so
/// no two can claim the same corner, and the two banners that flank it.
///
/// The pairing request notifier v2 also mounts here belongs to the pairing
/// surface, which owns its own card; the column is the placement, not the
/// inventory.
#[component]
pub fn NotificationDock() -> Element {
    use_md_stylesheet(NOTIFICATION_DOCK_STYLESHEET);
    // The hover ring is provided by `GatedApp`, above the gate: this dock and
    // the shell whose rows are meant to ring are SIBLINGS, so a target
    // provided here could never reach them.
    let compact = crate::components::layout::window_size::use_is_compact();
    // The column is ALWAYS mounted, empty or not. A card's bottom edge is where
    // the dock's own bottom edge is, and an unmounted dock has no edge to
    // measure — so a caller that has to prove the dock rides above the
    // composer can only do it against a column that is on the page.
    let composer = crate::components::terminal_chrome::composer_geometry::published_geometry();
    let lift = crate::components::layout::notification_dock_lift::notification_dock_lift(
        composer, compact,
    );
    rsx! {
        connection_banner::ConnectionBanner {}
        div {
            class: "roost-notify-dock",
            "data-testid": "notification-dock",
            "data-compact": if compact { "true" } else { "false" },
            style: "--roost-notify-dock-lift: {lift};",
            toast_stack::ToastStack {}
            undo_banner::UndoCloseBanner {}
            transfers_panel::TransfersPanel {}
            pad_hint_bar::PadHintBar {}
        }
        version_banner::VersionBanner {}
        agent_notifications::AgentNotifications {}
        agent_notifications::AgentAttention {}
    }
}
