//! The bottom notification dock: one stack of toasts, undo cards, transfer
//! cards and the new-build notice, beside the controller legend and the
//! connection banner. Mounted once by `app::AuthorizedOverlays`; every child
//! reads the client's toast, transfer and pending-close state and performs the
//! acknowledgement its own card rule names.

pub mod command_finished;
pub mod command_finished_scheduler;
pub mod connection_banner;
pub mod desktop_push_bridge;
pub mod dock_entries;
pub mod inline_transfer;
pub mod notification_tone;
pub mod notify_target;
pub mod pad_hint_bar;
pub mod page_attention;
pub mod store_write;
pub mod terminal_bell;
pub mod terminal_clipboard;
pub mod terminal_notification;
pub mod toast_card;
pub mod toast_stack;
pub mod transfer_outcome;
pub mod transfer_toast;
pub mod undo_banner;
pub mod version_banner;

use dioxus::prelude::*;

/// The one overlay column that owns where every transient notification sits, so
/// no two can claim the same corner, beside the connection banner.
///
/// The pairing request notifier belongs to the pairing surface, which owns its
/// own card; the column is the placement, not the inventory.
#[component]
pub fn NotificationDock() -> Element {
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
            class: "fixed z-[1100] flex flex-col items-end gap-2 pointer-events-none left-[calc(var(--roost-main-left)_+_env(safe-area-inset-left,0px)_+_0.75rem)] right-[calc(env(safe-area-inset-right,0px)_+_0.75rem)] bottom-[calc(var(--roost-notify-dock-lift))] tv:!left-[calc(var(--roost-main-left)_+_var(--tv-overscan-inline)_+_0.75rem)] tv:!right-[calc(var(--tv-overscan-inline)_+_0.75rem)] tv:!bottom-[calc(var(--roost-notify-dock-lift)_+_var(--tv-overscan-block))] [&>:not(.pad-hint)]:pointer-events-auto [&>:not(.pad-hint)]:w-[min(var(--roost-toast-inline-size),100%)] data-[compact=true]:[&>:not(.pad-hint)]:w-full",
            "data-testid": "notification-dock",
            "data-compact": if compact { "true" } else { "false" },
            style: "--roost-notify-dock-lift: {lift};",
            toast_stack::ToastStack {}
            pad_hint_bar::PadHintBar {}
        }
        terminal_clipboard::TerminalClipboard {}
        command_finished::CommandFinished {}
        terminal_bell::TerminalBellPresenter {}
        terminal_notification::TerminalNotification {}
        desktop_push_bridge::DesktopPushBridge {}
    }
}
