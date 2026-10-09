//! The pane switch: one id in, that pane's editor out.
//!
//! Ports `apps/web/src/components/Settings/SettingsPane.tsx`, which is exactly
//! this table. An id with no arm renders nothing, which is unreachable from the
//! rail (which only offers registered ids) and from `SettingsSurface` (which
//! falls back to the rail's first pane before reaching here) — but keeping the
//! mapping total means a retired pane's old bookmark shows the default editor
//! rather than an empty box.

use dioxus::prelude::*;

use super::agent_launcher::AgentLauncherPane;
use super::agent_models::AgentModelsPane;
use super::attachments::AttachmentsPane;
use super::audit::AuditLogPane;
use super::connection::ConnectionPane;
use super::devices::DevicesPane;
use super::machines::MachinesPane;
use super::mcp::McpPane;
use super::metrics::MetricsPane;
use super::notifications::NotificationsPane;
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
            "models" => rsx! { AgentModelsPane {} },
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
