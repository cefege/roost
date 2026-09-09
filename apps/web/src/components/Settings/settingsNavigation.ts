// Canonical Settings navigation: one rail definition for every deployment.
// Pane IDs here drive the rail, the mobile list, and the unknown-pane fallback;
// retired permissions/webhook panes intentionally have no compatibility alias.
export interface SettingsPaneSpec {
  id: string;
  label: string;
  icon: string;
  title: string;
}

export interface SettingsRailGroup {
  label: string;
  panes: readonly SettingsPaneSpec[];
}

const MACHINES_PANE: SettingsPaneSpec = {
  id: "machines",
  label: "Machines",
  icon: "desktop_mac",
  title: "Machines",
};

const CONNECTION_PANE: SettingsPaneSpec = {
  id: "connection",
  label: "Connection",
  icon: "lan",
  title: "Connection",
};

const DEVICES_PANE: SettingsPaneSpec = {
  id: "devices",
  label: "Devices",
  icon: "devices",
  title: "Devices",
};

const SHARED_GROUPS: readonly SettingsRailGroup[] = [
  { label: "Agents", panes: [
    { id: "launcher", label: "Launcher", icon: "rocket_launch", title: "Default agent" },
    { id: "mcp", label: "MCP", icon: "extension", title: "MCP relays" },
  ] },
  { label: "Interface", panes: [
    { id: "terminal", label: "Terminal", icon: "terminal", title: "Terminal" },
    { id: "voice", label: "Voice", icon: "mic", title: "Voice dictation" },
    { id: "theme", label: "Theme", icon: "palette", title: "Theme" },
    { id: "notifications", label: "Notifications", icon: "notifications", title: "Notifications" },
  ] },
  { label: "System", panes: [
    { id: "attachments", label: "Files", icon: "folder_open", title: "Attachments" },
    { id: "audit", label: "Audit", icon: "history", title: "Audit log" },
    { id: "metrics", label: "Metrics", icon: "monitoring", title: "Metrics" },
  ] },
];

/** Rail order is the navigation contract: Network first, then the rest. */
export const SETTINGS_GROUPS: readonly SettingsRailGroup[] = [
  { label: "Network", panes: [MACHINES_PANE, CONNECTION_PANE, DEVICES_PANE] },
  ...SHARED_GROUPS,
];
