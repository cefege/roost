//! The canonical Settings navigation: one rail definition for every deployment.
//! Ported from `apps/web/src/components/Settings/settingsNavigation.ts`; read by
//! the `/design` settings-navigation specimen and by the Settings shell (the
//! SETTINGS slice), which drives the rail, the mobile list and the unknown-pane
//! fallback from these ids. Retired panes intentionally have no alias.

/// One settings pane: its route id, rail label, icon and page title.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsPaneSpec {
    /// The `/settings/:pane` segment.
    pub id: &'static str,
    /// The rail label.
    pub label: &'static str,
    /// The rail icon ligature.
    pub icon: &'static str,
    /// The pane's title bar text.
    pub title: &'static str,
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
    icon: &'static str,
    title: &'static str,
) -> SettingsPaneSpec {
    SettingsPaneSpec {
        id,
        label,
        icon,
        title,
    }
}

/// Rail order is the navigation contract: Network first, then the rest.
pub const SETTINGS_GROUPS: [SettingsRailGroup; 4] = [
    SettingsRailGroup {
        label: "Network",
        panes: &[
            pane("machines", "Machines", "desktop_mac", "Machines"),
            pane("connection", "Connection", "lan", "Connection"),
            pane("devices", "Devices", "devices", "Devices"),
        ],
    },
    SettingsRailGroup {
        label: "Agents",
        panes: &[
            pane("launcher", "Launcher", "rocket_launch", "Default agent"),
            pane("mcp", "MCP", "extension", "MCP relays"),
        ],
    },
    SettingsRailGroup {
        label: "Interface",
        panes: &[
            pane("terminal", "Terminal", "terminal", "Terminal"),
            pane("voice", "Voice", "mic", "Voice dictation"),
            pane("theme", "Theme", "palette", "Theme"),
            pane(
                "notifications",
                "Notifications",
                "notifications",
                "Notifications",
            ),
        ],
    },
    SettingsRailGroup {
        label: "System",
        panes: &[
            pane("attachments", "Files", "folder_open", "Attachments"),
            pane("audit", "Audit", "history", "Audit log"),
            pane("metrics", "Metrics", "monitoring", "Metrics"),
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
