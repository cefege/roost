// Agent launcher persistence failure browser regression.
// Drives the real Material switch because its internal selected state can only
// be proven restored after the coordinator request rejects in a browser.
// Depends on the terminal smoke stack and the settings launcher pane.

import { expect, test } from "./fixtures.ts";

test("rejected auto-launch update restores the native switch selection", async ({ smokePage, stack }) => {
  await smokePage.goto(`${stack.baseUrl}/settings/launcher`, { waitUntil: "domcontentloaded" });
  const autoLaunchToggle = smokePage.getByTestId("agent-auto-launch-toggle");
  await expect(autoLaunchToggle).toBeVisible();

  const initialSelection = await autoLaunchToggle.evaluate(
    (element) => (element as HTMLElement & { selected: boolean }).selected,
  );
  const pageErrors: Error[] = [];
  smokePage.on("pageerror", (error) => pageErrors.push(error));

  let rejectedUpdateRequests = 0;
  await smokePage.route("**/roost.v1.CoordinatorService/AgentConfigSet", async (route) => {
    rejectedUpdateRequests++;
    await route.abort("failed");
  });

  await autoLaunchToggle.click();

  await expect.poll(() => rejectedUpdateRequests).toBe(1);
  await expect(smokePage.getByTestId("toast")).toContainText("Auto-launch save failed:");
  await expect(autoLaunchToggle).toHaveJSProperty("selected", initialSelection);
  expect(pageErrors).toEqual([]);
});
