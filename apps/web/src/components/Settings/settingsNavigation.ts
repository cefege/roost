// Canonical Settings navigation for self-hosted and managed deployments.
// Pane IDs here drive the rail, unknown-pane fallback, and visibility tests;
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

const ACCOUNT_PANE: SettingsPaneSpec = {
  id: "account",
  label: "Account",
  icon: "account_circle",
  title: "Account",
};

const ORGANIZATION_PANE: SettingsPaneSpec = {
  id: "organization",
  label: "Organization",
  icon: "domain",
  title: "Organization",
};

const DASHBOARD_PANE: SettingsPaneSpec = {
  id: "dashboard",
  label: "Dashboard",
  icon: "dashboard",
  title: "Dashboard",
};

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

const SELF_HOSTED_GROUPS: readonly SettingsRailGroup[] = [
  { label: "Scope", panes: [ORGANIZATION_PANE, DASHBOARD_PANE] },
  { label: "Network", panes: [MACHINES_PANE, CONNECTION_PANE, DEVICES_PANE] },
  ...SHARED_GROUPS,
];

const MANAGED_GROUPS: readonly SettingsRailGroup[] = [
  { label: "Scope", panes: [ACCOUNT_PANE] },
  { label: "Network", panes: [MACHINES_PANE] },
  ...SHARED_GROUPS,
];

const MANAGED_HIDDEN_PANES: Readonly<Record<string, true>> = {
  [ORGANIZATION_PANE.id]: true,
  [DASHBOARD_PANE.id]: true,
  [CONNECTION_PANE.id]: true,
  [DEVICES_PANE.id]: true,
};

/** Stable, precomputed navigation groups for the active deployment profile. */
export function settingsGroupsForMode(managed: boolean): readonly SettingsRailGroup[] {
  return managed ? MANAGED_GROUPS : SELF_HOSTED_GROUPS;
}

/** Old/self-hosted scope URLs must not render their hidden pane in managed mode. */
export function resolveSettingsPaneForMode(pane: string, managed: boolean): string {
  return managed && MANAGED_HIDDEN_PANES[pane] === true ? ACCOUNT_PANE.id : pane;
}

export function isHiddenManagedSettingsPane(pane: string | undefined): boolean {
  return pane !== undefined && MANAGED_HIDDEN_PANES[pane] === true;
}

/** The organization/dashboard picker is a self-hosted-only surface. */
export function settingsScopeSelectorVisible(saasMode: boolean | null | undefined): boolean {
  return saasMode === false;
}
