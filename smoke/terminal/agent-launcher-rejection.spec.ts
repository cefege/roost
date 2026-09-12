// Agent launcher persistence failure browser regression.
// Covers rollback of controlled native switch and Kobalte select after one
// rejected coordinator write. Depends on the terminal smoke stack and settings launcher pane.

import { expect, test } from "./fixtures.ts";

test("rejected auto-launch update restores native switch checked state", async ({ smokePage, stack }) => {
  await smokePage.goto(`${stack.baseUrl}/settings/launcher`, { waitUntil: "domcontentloaded" });
  const autoLaunchToggle = smokePage.getByTestId("agent-auto-launch-toggle");
  await expect(autoLaunchToggle).toBeVisible();
  const initiallyChecked = await autoLaunchToggle.isChecked();
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
  if (initiallyChecked) {
    await expect(autoLaunchToggle).toBeChecked();
  } else {
    await expect(autoLaunchToggle).not.toBeChecked();
  }
  expect(rejectedUpdateRequests).toBe(1);
  expect(pageErrors).toEqual([]);
});

test("rejected default agent selection restores semantic select label", async ({ smokePage, stack }) => {
  await smokePage.goto(`${stack.baseUrl}/settings/launcher`, { waitUntil: "domcontentloaded" });
  const agentSelect = smokePage.getByTestId("agent-select");
  await expect(agentSelect).toBeVisible();
  const initialAgentLabel = (await agentSelect.locator(".roost-select__value").innerText()).trim();
  if (!initialAgentLabel) throw new Error("agent select has no visible initial label");
  const differentBuiltInAgent = initialAgentLabel === "OpenAI Codex"
    ? "Gemini CLI"
    : "OpenAI Codex";
  const pageErrors: Error[] = [];
  smokePage.on("pageerror", (error) => pageErrors.push(error));

  let rejectedUpdateRequests = 0;
  await smokePage.route("**/roost.v1.CoordinatorService/AgentConfigSet", async (route) => {
    rejectedUpdateRequests++;
    await route.abort("failed");
  });

  await agentSelect.click();
  const agentOptions = smokePage.getByRole("listbox");
  await expect(agentOptions).toBeVisible();
  const differentAgentOption = agentOptions.getByRole("option", {
    name: differentBuiltInAgent,
    exact: true,
  });
  await expect(differentAgentOption).toBeVisible();
  await differentAgentOption.click();

  await expect.poll(() => rejectedUpdateRequests).toBe(1);
  await expect(smokePage.getByTestId("toast")).toContainText("Default agent save failed:");
  await expect(agentOptions).not.toBeVisible();
  await expect(agentSelect.locator(".roost-select__value")).toHaveText(initialAgentLabel);
  expect(rejectedUpdateRequests).toBe(1);
  expect(pageErrors).toEqual([]);
});
