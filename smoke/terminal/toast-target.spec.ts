// A session-targeted toast points at where its View action lands: hovering rings
// the target's sidebar folder row when no pane tab represents it, and the desktop
// card sizes to its content instead of the dock's full measure.

import { test, expect } from "./fixtures.ts";
import {
  launchIntegratedAgent,
  pollAgentStatus,
  reportAgentStatus as report,
  type AgentStatusSmoke,
} from "./agent-status-fixture.ts";

// 52ch at the theme's body measure; the assertion only needs the cap to be well
// under the dock's 70ch, not an exact glyph width.
const TOAST_MEASURE_CAP_PX = 480;

test("a hovered agent toast rings its off-screen target and sizes to content", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop hover and sidebar surfaces");
  test.setTimeout(180_000);

  const [activeId, remoteFolderId] = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: AgentStatusSmoke }).__smoke;
    const active = await smoke.spawnShell(workerFp, "/tmp");
    const remote = await smoke.spawnShell(workerFp, "/");
    smoke.forceVisible(true);
    return [active.session_id, remote.session_id];
  }, stack.workerFp);

  await smokePage.goto(`${stack.baseUrl}/s/${activeId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${activeId}`)).toBeVisible();
  await smokePage.getByTestId("sidebar-view-folders").click();

  await launchIntegratedAgent(smokePage, remoteFolderId);
  await pollAgentStatus(
    stack.client,
    remoteFolderId,
    "idle",
    (status) => status.source === "screen",
  );
  // Only a working→blocked transition produces a "needs your input" toast
  // (classifyAgentTransition), and the SPA must observe the working state
  // before the blocked report lands — the target's folder row proves it did.
  const remoteFolderStatus = smokePage.locator('[data-testid^="folder-agent-status-"]').first();
  await report(remoteFolderId, "working", 1);
  await expect(remoteFolderStatus).toHaveAttribute("data-level", "working", { timeout: 30_000 });
  await report(remoteFolderId, "blocked", 2, true, "Approval needed");

  const toast = smokePage.getByTestId("toast").filter({ hasText: "needs your input" });
  await expect(toast).toBeVisible({ timeout: 10_000 });
  // The target sits in another folder, so the on-screen tab strip cannot
  // represent it — the sidebar row is the only surface left to ring.
  await expect(smokePage.getByTestId(`tab-${remoteFolderId}`)).toHaveCount(0);

  await toast.hover();
  // Scoped to :visible because SidebarRoot keeps both sidebar panels mounted and
  // hides the inactive one with visibility:hidden — the agent row of the
  // off-screen Agents panel carries the attribute but paints nothing.
  const ringed = smokePage.locator('[data-notify-target="true"]:visible');
  await expect(ringed).toHaveCount(1);
  await expect(ringed).toHaveAttribute("data-testid", /^folder-row-/);
  // The same hover froze auto-dismiss; the countdown bar is keyed on this
  // attribute so the bar and the JS timer cannot disagree.
  await expect(toast).toHaveAttribute("data-dismiss-held", "true");

  const toastBox = (await toast.boundingBox())!;
  const dockBox = (await smokePage.getByTestId("notification-dock").boundingBox())!;
  expect(toastBox.width).toBeLessThan(dockBox.width);
  expect(toastBox.width).toBeLessThanOrEqual(TOAST_MEASURE_CAP_PX);

  // Leaving clears both the ring and the freeze.
  await smokePage.mouse.move(0, 0);
  await expect(smokePage.locator('[data-notify-target="true"]')).toHaveCount(0);
  await expect(toast).not.toHaveAttribute("data-dismiss-held", "true");
});
