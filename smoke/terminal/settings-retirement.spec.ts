import { expect, test } from "./fixtures.ts";

test("retired Settings panes stay absent while MCP remains available", async ({ smokePage }) => {
  await smokePage.setViewportSize({ width: 1440, height: 900 });
  await smokePage.goto(new URL("/settings", smokePage.url()).href, {
    waitUntil: "domcontentloaded",
  });
  const machinesRailItem = smokePage.getByTestId("rail-machines");
  await expect(smokePage.locator(".settings-rail")).toBeVisible();
  await expect(machinesRailItem).toHaveAttribute("href", "/settings/machines");
  await expect(machinesRailItem).toHaveAttribute("data-selected", "true");
  await expect(machinesRailItem).toHaveAttribute("aria-current", "page");
  await expect(smokePage).toHaveURL(/\/settings$/);
  await expect(smokePage.getByText("Permissions", { exact: true })).toHaveCount(0);
  await expect(smokePage.getByText("Webhooks", { exact: true })).toHaveCount(0);

  const mcpRailItem = smokePage.getByTestId("rail-mcp");
  await mcpRailItem.click();
  await expect(smokePage).toHaveURL(/\/settings\/mcp$/);
  await expect(machinesRailItem).not.toHaveAttribute("data-selected", "true");
  await expect(machinesRailItem).not.toHaveAttribute("aria-current", "page");
  await expect(mcpRailItem).toHaveAttribute("data-selected", "true");
  await expect(mcpRailItem).toHaveAttribute("aria-current", "page");
  await expect(smokePage.getByTestId("settings-mcp-pane")).toBeVisible();
});
