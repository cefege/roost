//! Settings editor dispatch for registered pane IDs.
//! Called by `SettingsSurface`, which validates IDs against the navigation registry.
//! Native editor modules are retained but are not mounted by this switch.

use dioxus::prelude::*;

use super::attachments::AttachmentsPane;
use super::audit::AuditLogPane;
use super::devices::DevicesPane;
use super::machines::MachinesPane;
use super::metrics::MetricsPane;
use super::notifications::NotificationsPane;
use super::terminal::TerminalPane;
use super::theme::ThemePane;
use super::voice::VoicePane;
use crate::components::access::AccessSurface;

/// The editor for one settings pane.
#[component]
pub fn SettingsPane(id: &'static str) -> Element {
    rsx! {
        match id {
            "machines" => rsx! { MachinesPane {} },
            "connection" => rsx! { AccessSurface {} },
            "devices" => rsx! { DevicesPane {} },
            "voice" => rsx! { VoicePane {} },
            "terminal" => rsx! { TerminalPane {} },
            "notifications" => rsx! { NotificationsPane {} },
            "attachments" => rsx! { AttachmentsPane {} },
            "theme" => rsx! { ThemePane {} },
            "audit" => rsx! { AuditLogPane {} },
            "metrics" => rsx! { MetricsPane {} },
            _ => rsx! {},
        }
    }
}
