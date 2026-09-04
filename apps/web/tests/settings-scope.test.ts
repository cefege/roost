import { describe, expect, test } from "bun:test";
import {
  isHiddenManagedSettingsPane,
  resolveSettingsPaneForMode,
  settingsGroupsForMode,
  settingsScopeSelectorVisible,
} from "../src/components/Settings/settingsNavigation.ts";

function paneIds(managed: boolean): string[] {
  return settingsGroupsForMode(managed).flatMap((group) => group.panes.map((pane) => pane.id));
}

describe("managed Settings scope", () => {
  test("replaces every multi-scope and standalone device surface with one Account pane", () => {
    const groups = settingsGroupsForMode(true);
    const ids = paneIds(true);

    expect(groups[0]?.panes.map((pane) => pane.id)).toEqual(["account"]);
    expect(ids.filter((id) => id === "account")).toHaveLength(1);
    expect(ids).not.toContain("organization");
    expect(ids).not.toContain("dashboard");
    expect(ids).not.toContain("connection");
    expect(ids).not.toContain("devices");
    expect(groups.find((group) => group.label === "Network")?.panes.map((pane) => pane.id))
      .toEqual(["machines"]);
    expect(settingsScopeSelectorVisible(true)).toBe(false);
    expect(settingsScopeSelectorVisible(undefined)).toBe(false);
  });

  test("redirects direct hidden-pane URLs to Account without redirecting shared panes", () => {
    for (const pane of ["organization", "dashboard", "connection", "devices"]) {
      expect(isHiddenManagedSettingsPane(pane)).toBe(true);
      expect(resolveSettingsPaneForMode(pane, true)).toBe("account");
    }

    expect(resolveSettingsPaneForMode("account", true)).toBe("account");
    expect(resolveSettingsPaneForMode("machines", true)).toBe("machines");
    expect(isHiddenManagedSettingsPane(undefined)).toBe(false);
  });
});

describe("retired Settings surfaces", () => {
  test("keeps MCP under Agents and leaves retired pane URLs to the unknown-pane fallback", () => {
    for (const managed of [false, true]) {
      const groups = settingsGroupsForMode(managed);
      const ids = paneIds(managed);
      expect(groups.find((group) => group.label === "Agents")?.panes.map((pane) => pane.id))
        .toEqual(["launcher", "mcp"]);
      expect(ids).not.toContain("permissions");
      expect(ids).not.toContain("webhooks");
    }

    for (const pane of ["permissions", "webhooks"]) {
      expect(isHiddenManagedSettingsPane(pane)).toBe(false);
      expect(resolveSettingsPaneForMode(pane, true)).toBe(pane);
      expect(resolveSettingsPaneForMode(pane, false)).toBe(pane);
    }
  });
});

describe("self-hosted Settings scope", () => {
  test("retains organization, dashboard, connection, device pairing, and switching surfaces", () => {
    const groups = settingsGroupsForMode(false);

    expect(groups.find((group) => group.label === "Scope")?.panes.map((pane) => pane.id))
      .toEqual(["organization", "dashboard"]);
    expect(groups.find((group) => group.label === "Network")?.panes.map((pane) => pane.id))
      .toEqual(["machines", "connection", "devices"]);
    expect(paneIds(false)).not.toContain("account");
    expect(settingsScopeSelectorVisible(false)).toBe(true);
    expect(resolveSettingsPaneForMode("devices", false)).toBe("devices");
  });
});
