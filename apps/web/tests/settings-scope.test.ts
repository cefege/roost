import { describe, expect, test } from "bun:test";
import { SETTINGS_GROUPS } from "../src/components/Settings/settingsNavigation.ts";

const PANE_IDS = SETTINGS_GROUPS.flatMap((group) => group.panes.map((pane) => pane.id));

describe("Settings navigation", () => {
  test("keeps scope, network, and agent surfaces in rail order", () => {
    expect(SETTINGS_GROUPS.find((group) => group.label === "Scope")?.panes.map((pane) => pane.id))
      .toEqual(["organization", "dashboard"]);
    expect(SETTINGS_GROUPS.find((group) => group.label === "Network")?.panes.map((pane) => pane.id))
      .toEqual(["machines", "connection", "devices"]);
    expect(SETTINGS_GROUPS.find((group) => group.label === "Agents")?.panes.map((pane) => pane.id))
      .toEqual(["launcher", "mcp"]);
  });

  test("retired panes have no rail entry and no compatibility alias", () => {
    for (const pane of ["account", "permissions", "webhooks"]) {
      expect(PANE_IDS, pane).not.toContain(pane);
    }
  });

  test("every pane id is unique so the rail cannot render a duplicate row", () => {
    expect(new Set(PANE_IDS).size).toBe(PANE_IDS.length);
  });
});
