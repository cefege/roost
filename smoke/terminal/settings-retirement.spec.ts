import { expect, test } from "./fixtures.ts";

test("retired Settings panes stay absent while MCP remains available", async ({ smokePage }) => {
  await smokePage.goto(new URL("/settings/mcp", smokePage.url()).href, {
    waitUntil: "domcontentloaded",
  });
  await expect(smokePage.getByTestId("settings-mcp-pane")).toBeVisible();
  await expect(smokePage.getByRole("button", { name: "MCP", exact: true })).toBeVisible();
  await expect(smokePage.getByText("Permissions", { exact: true })).toHaveCount(0);
  await expect(smokePage.getByText("Webhooks", { exact: true })).toHaveCount(0);
});
