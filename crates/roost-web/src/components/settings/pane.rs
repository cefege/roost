//! Settings editor dispatch for registered pane IDs.
//! Called by `SettingsSurface`, which validates IDs against the navigation registry.
//! Native editor modules are retained but are not mounted by this switch.

use dioxus::prelude::*;

use super::agent_launcher::AgentLauncherPane;
use super::attachments::AttachmentsPane;
use super::audit::AuditLogPane;
use super::connection::ConnectionPane;
use super::devices::DevicesPane;
use super::machines::MachinesPane;
use super::mcp::McpPane;
use super::metrics::MetricsPane;
use super::notifications::NotificationsPane;
use super::omp_settings::OmpSettingsPane;
use super::terminal::TerminalPane;
use super::theme::ThemePane;
use super::voice::VoicePane;

/// The editor for one settings pane.
#[component]
pub fn SettingsPane(id: &'static str) -> Element {
    rsx! {
        match id {
            "machines" => rsx! { MachinesPane {} },
            "connection" => rsx! { ConnectionPane {} },
            "devices" => rsx! { DevicesPane {} },
            "omp" => rsx! { OmpSettingsPane {} },
            "launcher" => rsx! { AgentLauncherPane {} },
            "mcp" => rsx! { McpPane {} },
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
