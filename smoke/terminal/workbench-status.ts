// Verifies the workbench status bar against independent coordinator state.
// The shell spec owns geometry and navigation; this helper keeps status truth
// reusable without inflating the scenario file beyond the repository cap.

import type { Page } from "@playwright/test";
import type { TerminalTestStack } from "./stack.ts";
import { expect } from "./fixtures.ts";

export async function expectStatusTruth(page: Page, stack: TerminalTestStack): Promise<void> {
  const status = page.getByTestId("workbench-status-bar");
  await expect(status).toBeVisible();
  await expect.poll(() => status.getByTestId("workbench-status-sync").textContent())
    .toContain("Synced");
  await expect(status.getByTestId("workbench-status-worker")).toContainText("roost-terminal-test");
  const [{ sessions }, { workers, routableFps }] = await Promise.all([
    stack.client.sessionsList({ status: "all" }),
    stack.client.workersList({}),
  ]);
  const openCount = sessions.filter((session) => session.status === "open").length;
  const routable = new Set(routableFps);
  const onlineCount = workers.filter((worker) => routable.has(worker.fp)).length;
  const counts = status.getByTestId("workbench-status-counts");
  await expect(counts).toContainText(`${openCount} ${openCount === 1 ? "session" : "sessions"}`);
  await expect(counts).toContainText(`${onlineCount}/${workers.length} workers`);
  await expect(status.getByTestId("workbench-status-agent")).toHaveCount(0);
  const revision = await status.getByTestId("workbench-status-revision").textContent();
  expect(revision).toMatch(/^[0-9a-f]{7}$/);
}
