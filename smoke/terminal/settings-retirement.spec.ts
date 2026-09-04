import { expect, test } from "./fixtures.ts";

test("retired Settings panes stay absent while MCP remains available", async ({ smokePage }) => {
  await smokePage.goto(new URL("/settings", smokePage.url()).href, {
    waitUntil: "domcontentloaded",
  });
  const mcpRoute = smokePage.getByRole("button", { name: "MCP", exact: true });
  await expect(mcpRoute).toBeVisible();
  await expect(smokePage.getByText("Permissions", { exact: true })).toHaveCount(0);
  await expect(smokePage.getByText("Webhooks", { exact: true })).toHaveCount(0);
  await mcpRoute.click();
  await expect(smokePage.getByTestId("settings-mcp-pane")).toBeVisible();
});
