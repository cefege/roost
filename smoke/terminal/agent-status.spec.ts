// Coding-agent status path, end to end without model credentials:
// spawned PTY report -> worker/coord/Sync -> session, folder, notification,
// and route-driven attention surfaces, including acknowledgement decay.

import { test, expect } from "./fixtures.ts";
import {
  launchIntegratedAgent,
  pollAgentStatus,
  reportAgentStatus as report,
  reportReplacementAgentStatus,
  type AgentStatusSmoke,
} from "./agent-status-fixture.ts";

const UUID_PATTERN =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;

test("agent status reaches every browser surface and notification ACK", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop status surfaces");
  test.setTimeout(180_000);

  const [activeId, backgroundId] = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    const first = await smoke.spawnShell(workerFp, "/tmp");
    const second = await smoke.spawnShell(workerFp, "/tmp");
    smoke.forceVisible(true);
    return [first.session_id, second.session_id];
  }, stack.workerFp);

  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${activeId}`)).toBeVisible();
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toBeVisible();

  const tabStatus = smokePage
    .getByTestId(`tab-${backgroundId}`)
    .getByTestId(`agent-status-${backgroundId}`);
  const folderStatus = smokePage.locator('[data-testid^="folder-agent-status-"]').first();

  // One report is checked for its socket ACK so a silently rejected report
  // (bad pid ownership, unwritable socket) can't masquerade as a UI bug; the
  // remaining reports are proven by the surfaces they move.
  await launchIntegratedAgent(smokePage, backgroundId);
  await pollAgentStatus(
    stack.client,
    backgroundId,
    "idle",
    (status) => status.source === "screen",
  );
  await report(backgroundId, "working", 1);
  await smokePage.goto(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${backgroundId}`)).toContainText(
    `STATUS_ACK_1 {"ok":true}`,
    { timeout: 30_000 },
  );
  const firstStatus = await pollAgentStatus(stack.client, backgroundId, "working");
  expect(firstStatus).toMatchObject({
    sessionId: backgroundId,
    agentId: "omp",
    state: "working",
    source: "integration",
    promptable: true,
  });
  expect(firstStatus.statusEpoch).toMatch(UUID_PATTERN);
  expect(firstStatus.occupantId).toMatch(UUID_PATTERN);
  const sameShellEpoch = firstStatus.statusEpoch!;
  const sameShellOccupant = firstStatus.occupantId!;
  const serializedPublicStatus = JSON.stringify(firstStatus, (_key, value) =>
    typeof value === "bigint" ? value.toString() : value
  );
  expect(serializedPublicStatus).not.toMatch(/"pid"\s*:/i);
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "working");
  await expect(folderStatus).toContainText("1 working");

  // Search switches the sidebar from folder rows to the full per-session row
  // without changing the active terminal. This locks the distinct session-row
  // surface as well as the folder rollup above.
  await smokePage.getByTestId("brand-row-search").click();
  await smokePage.getByTestId("sidebar-search").fill("/tmp");
  const sessionRow = smokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${backgroundId}"]`,
  );
  await expect(sessionRow).toBeVisible();
  await expect(sessionRow.getByTestId(`agent-status-${backgroundId}`)).toHaveAttribute(
    "data-level",
    "working",
  );
  await smokePage.getByTestId("sidebar-search-clear").click();
  await expect(folderStatus).toBeVisible();

  await report(backgroundId, "blocked", 2, true, "Approval needed");
  await expect(tabStatus).toHaveAttribute("data-level", "blocked", { timeout: 30_000 });
  await expect(folderStatus).toHaveAttribute("data-level", "blocked");
  await expect(folderStatus).toContainText("1 needs input");
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);
  const blockedToast = smokePage.getByTestId("toast").filter({ hasText: "needs your input" });
  await expect(blockedToast).toBeVisible({ timeout: 10_000 });
  await expect(blockedToast.getByTestId("toast-details")).toContainText("Approval needed");
  const blockedStatus = await pollAgentStatus(stack.client, backgroundId, "blocked");
  expect([blockedStatus.statusEpoch, blockedStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/search?scope=attention`);
  const blockedAttention = smokePage.getByTestId(`global-search-result-${backgroundId}`);
  await expect(blockedAttention).toBeVisible();
  await expect(
    blockedAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Needs input");
  await blockedAttention.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(smokePage.getByTestId(`tab-${backgroundId}`)).toHaveAttribute("data-active", "true");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  // Viewing acknowledges the notification revision, but current blocked state
  // remains attention until the agent itself transitions.
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(blockedAttention).toBeVisible();
  await expect(
    blockedAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Needs input");

  // Move away before completion so the transition is genuinely backgrounded.
  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`tab-${activeId}`)).toHaveAttribute("data-active", "true");
  await report(backgroundId, "working", 3);
  await expect(tabStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  const resumedStatus = await pollAgentStatus(stack.client, backgroundId, "working");
  expect([resumedStatus.statusEpoch, resumedStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);
  await report(backgroundId, "idle", 4);
  await expect(tabStatus).toHaveAttribute("data-level", "done", { timeout: 30_000 });
  const idleStatus = await pollAgentStatus(stack.client, backgroundId, "idle");
  expect([idleStatus.statusEpoch, idleStatus.occupantId]).toEqual([
    sameShellEpoch,
    sameShellOccupant,
  ]);
  await expect(folderStatus).toHaveAttribute("data-level", "done");
  await expect(folderStatus).toContainText("1 done");
  const doneToast = smokePage.getByTestId("toast").filter({ hasText: "finished" });
  await expect(doneToast).toBeVisible({ timeout: 10_000 });
  await expect.poll(() => smokePage.title()).toMatch(/^\(1\) Roost/);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  const doneAttention = smokePage.getByTestId(`global-search-result-${backgroundId}`);
  await expect(doneAttention).toBeVisible();
  await expect(
    doneAttention.getByTestId(`global-search-attention-${backgroundId}`),
  ).toHaveText("Done");
  await doneAttention.click();
  await expect(smokePage).toHaveURL(`${stack.baseUrl}/s/${backgroundId}`);
  await expect(tabStatus).toHaveAttribute("data-level", "idle");
  await expect.poll(() => smokePage.title()).not.toMatch(/^\(\d+\)/);

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    smoke.navigate("/search?scope=attention");
  });
  await expect(doneAttention).toHaveCount(0);
  await expect(smokePage.getByText("Nothing needs attention", { exact: true })).toBeVisible();

  await report(backgroundId, "idle", 5, false);
  await expect(tabStatus).toHaveCount(0, { timeout: 30_000 });
  await expect(folderStatus).toHaveCount(0);

  await launchIntegratedAgent(smokePage, activeId);
  await pollAgentStatus(
    stack.client,
    activeId,
    "idle",
    (status) => status.source === "screen",
  );
  await report(activeId, "working", 1);
  const firstReplacementStatus = await pollAgentStatus(stack.client, activeId, "working");
  expect(firstReplacementStatus.statusEpoch).toBe(sameShellEpoch);
  expect(firstReplacementStatus.occupantId).toMatch(UUID_PATTERN);
  await launchIntegratedAgent(smokePage, activeId);
  const secondReplacementStatus = await reportReplacementAgentStatus(
    stack.client,
    activeId,
    firstReplacementStatus.occupantId!,
  );
  expect(secondReplacementStatus.statusEpoch).toBe(firstReplacementStatus.statusEpoch);
  expect(secondReplacementStatus.occupantId).toMatch(UUID_PATTERN);
  expect(secondReplacementStatus.occupantId).not.toBe(firstReplacementStatus.occupantId);
});
