//! The Settings destinations, their plain-language descriptions and rail order.
//! The Settings shell and `/design` specimen share this definition.
//! Route ids stay stable when labels change; retired panes have no aliases.

use crate::components::ui::IconType;

/// One settings pane: its route id, rail label, icon and page title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPaneSpec {
    /// The `/settings/:pane` segment.
    pub id: &'static str,
    /// The rail label.
    pub label: &'static str,
    /// The rail icon.
    pub icon: IconType,
    /// The pane's title bar text.
    pub title: &'static str,
    /// Explains the pane in search results and under its page title.
    pub description: &'static str,
    /// Control names and aliases that make the pane discoverable.
    pub search_terms: &'static str,
}

/// A labelled group of panes on the rail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsRailGroup {
    /// The group heading.
    pub label: &'static str,
    /// The panes, in rail order.
    pub panes: &'static [SettingsPaneSpec],
}

const fn pane(
    id: &'static str,
    label: &'static str,
    icon: IconType,
    title: &'static str,
    description: &'static str,
    search_terms: &'static str,
) -> SettingsPaneSpec {
    SettingsPaneSpec {
        id,
        label,
        icon,
        title,
        description,
        search_terms,
    }
}

/// Rail order is the navigation contract: Network first, then the rest.
pub const SETTINGS_GROUPS: [SettingsRailGroup; 4] = [
    SettingsRailGroup {
        label: "Network",
        panes: &[
            pane(
                "machines",
                "Machines",
                IconType::Laptop,
                "Machines",
                "Connected computers, folders and worker availability.",
                "workers computers",
            ),
            pane(
                "connection",
                "Connection",
                IconType::Network,
                "Connection",
                "Coordinator address, connection health and reconnect options.",
                "server offline reconnect",
            ),
            pane(
                "devices",
                "Paired devices",
                IconType::Smartphone,
                "Paired devices",
                "Browsers and phones that can access Roost. Review or revoke access.",
                "pairing browsers phones access",
            ),
        ],
    },
    SettingsRailGroup {
        label: "Agents",
        panes: &[
            pane(
                "launcher",
                "Agent launcher",
                IconType::Rocket,
                "Agent launcher",
                "The agent command used when opening a terminal session.",
                "omp command launch",
            ),
            pane(
                "models",
                "Models & accounts",
                IconType::Bot,
                "Models & accounts",
                "Models for chat and tasks, folder defaults, advisor reviews and provider accounts.",
                "advisor adviser planning delegated quick side tasks decisions fallback chains session thinking effort btw compact web search",
            ),
            pane(
                "mcp",
                "Tool integrations",
                IconType::Puzzle,
                "Tool integrations",
                "MCP relays that connect agents to external tools and services.",
                "external tools relay",
            ),
        ],
    },
    SettingsRailGroup {
        label: "Interface",
        panes: &[
            pane(
                "terminal",
                "Terminal",
                IconType::Terminal,
                "Terminal",
                "Font, cursor, rendering and terminal interaction.",
                "scrollback keyboard rendering",
            ),
            pane(
                "voice",
                "Voice",
                IconType::Mic,
                "Voice dictation",
                "Speech recognition, language and microphone input.",
                "dictation audio",
            ),
            pane(
                "theme",
                "Appearance",
                IconType::Palette,
                "Appearance",
                "Theme, color scheme and how the workbench looks.",
                "dark light colors",
            ),
            pane(
                "notifications",
                "Notifications",
                IconType::Bell,
                "Notifications",
                "Alerts when agents need attention or finish their work.",
                "sound alerts",
            ),
        ],
    },
    SettingsRailGroup {
        label: "System",
        panes: &[
            pane(
                "attachments",
                "Files",
                IconType::FolderOpen,
                "Files",
                "Uploaded files and session attachments.",
                "uploads downloads",
            ),
            pane(
                "audit",
                "Audit log",
                IconType::History,
                "Audit log",
                "Recorded access and administrative activity.",
                "security activity",
            ),
            pane(
                "metrics",
                "Diagnostics",
                IconType::Activity,
                "Diagnostics",
                "Service metrics and resource usage.",
                "health performance",
            ),
        ],
    },
];

/// The pane registered under an id, in any group.
pub fn settings_pane(id: &str) -> Option<&'static SettingsPaneSpec> {
    SETTINGS_GROUPS
        .iter()
        .flat_map(|group| group.panes.iter())
        .find(|pane| pane.id == id)
}
