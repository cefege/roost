//! The bottom notification column and the two banners that own the corners
//! around it: toasts, undo snackbars, the transfer popup, the controller
//! legend, plus the connection and version notices. Mounted once by
//! `app::AuthorizedOverlays`; every child reads the client's toast, transfer and
//! pending-close state and performs the acknowledgement its own card rule names.
//! Ports `apps/web/src/components/notifications/`.

pub mod agent_notifications;
pub mod command_finished;
pub mod command_finished_scheduler;
pub mod connection_banner;
pub mod desktop_push_bridge;
pub mod notify_target;
pub mod pad_hint_bar;
pub mod store_write;
pub mod terminal_bell;
pub mod terminal_clipboard;
pub mod terminal_notification;
pub mod toast_card;
pub mod toast_stack;
pub mod transfer_outcome;
pub mod transfer_row;
pub mod transfers_panel;
pub mod undo_banner;
pub mod version_banner;

use dioxus::prelude::*;
use libero::{components::Flex, sx::sx};

/// The one overlay column that owns where every transient notification sits, so
/// no two can claim the same corner, and the two banners that flank it.
///
/// The pairing request notifier v2 also mounts here belongs to the pairing
/// surface, which owns its own card; the column is the placement, not the
/// inventory.
#[component]
pub fn NotificationDock() -> Element {
    // The hover ring is provided by `GatedApp`, above the gate: this dock and
    // the shell whose rows are meant to ring are SIBLINGS, so a target
    // provided here could never reach them.
    let compact = crate::components::layout::window_size::use_is_compact();
    let television = crate::components::layout::window_size::use_tv_layout();
    let (ordinary_sx, television_sx) = use_hook(|| {
        let ordinary = sx()
            .position("fixed")
            .z_index("1100")
            .align_items("flex-end")
            .pointer_events("none")
            .with(
                "left",
                "calc(var(--roost-main-left) + env(safe-area-inset-left, 0px) + 0.75rem)",
            )
            .with("right", "calc(env(safe-area-inset-right, 0px) + 0.75rem)")
            .with("bottom", "var(--roost-notify-dock-lift)")
            .selector(
                "& > :not(.pad-hint)",
                sx().pointer_events("auto")
                    .width("min(var(--roost-toast-inline-size), 100%)"),
            )
            .selector(
                "&[data-compact='true'] > :not(.pad-hint)",
                sx().width("100%"),
            );
        let television = ordinary
            .clone()
            .with(
                "left",
                "calc(var(--roost-main-left) + var(--tv-overscan-inline) + 0.75rem)",
            )
            .with("right", "calc(var(--tv-overscan-inline) + 0.75rem)")
            .with(
                "bottom",
                "calc(var(--roost-notify-dock-lift) + var(--tv-overscan-block))",
            );
        (ordinary, television)
    });
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
        Flex {
            direction: "column", gap: "xs", sx: if television { television_sx } else { ordinary_sx },
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
        terminal_clipboard::TerminalClipboard {}
        command_finished::CommandFinished {}
        terminal_bell::TerminalBellPresenter {}
        terminal_notification::TerminalNotification {}
        desktop_push_bridge::DesktopPushBridge {}
    }
}
