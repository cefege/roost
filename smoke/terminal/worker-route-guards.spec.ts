// Browser proof for independently hydrated worker-route guards and denial replacements.
// It delays WorkersList behind a completed terminal snapshot, verifies focus ownership,
// then constrains the file sheet and reaches its recovery action by keyboard scrolling.

import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

interface WorkerRouteGuardWindow extends Window {
  readonly __smoke: RecoverySmokeApi;
}

test("worker routes wait for their domain and keep denial recovery reachable", async ({
  smokePage,
  stack,
}) => {
  await smokePage.setViewportSize({ width: 1024, height: 720 });

  const seedSessionId = await smokePage.evaluate(async ({ workerFp, folder }) => {
    const smokeWindow = window as unknown as WorkerRouteGuardWindow;
    return (await smokeWindow.__smoke.spawnShell(workerFp, folder)).session_id;
  }, { workerFp: stack.workerFp, folder: stack.workerHome });
  await smokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as WorkerRouteGuardWindow;
    return !!smokeWindow.__smoke?.state().sessions[sessionId];
  }, seedSessionId);

  const workersListStarted = Promise.withResolvers<void>();
  const releaseWorkersList = Promise.withResolvers<void>();
  const workersListFinished = Promise.withResolvers<void>();
  let workersListIntercepted = false;
  const workersListRoute = "**/roost.v1.CoordinatorService/WorkersList";
  await smokePage.route(workersListRoute, async (route) => {
    workersListIntercepted = true;
    workersListStarted.resolve();
    try {
      await releaseWorkersList.promise;
      await route.continue();
    } finally {
      workersListFinished.resolve();
    }
  });
  try {
    await smokePage.goto(`${stack.baseUrl}/browse/${stack.workerFp}`, {
      waitUntil: "domcontentloaded",
    });
    await workersListStarted.promise;
    await smokePage.waitForFunction((sessionId) => {
      const smokeWindow = window as unknown as WorkerRouteGuardWindow;
      return !!smokeWindow.__smoke?.state().sessions[sessionId];
    }, seedSessionId);
    await expect(smokePage.getByText("Loading machine…", { exact: true })).toBeVisible();
    await expect(smokePage.getByTestId("browse-worker-unavailable")).toHaveCount(0);
  } finally {
    releaseWorkersList.resolve();
    if (workersListIntercepted) await workersListFinished.promise.catch(() => undefined);
    await smokePage.unroute(workersListRoute);
  }
  await expect(smokePage.getByTestId("browse-open")).toBeVisible();
  const seedCleanup = await smokePage.evaluate(async () => {
    const smokeWindow = window as unknown as WorkerRouteGuardWindow;
    return smokeWindow.__smoke.cleanupCreated();
  });
  expect(seedCleanup.errors).toEqual([]);
  expect(seedCleanup.killedSessions).toContain(seedSessionId);

  const dashboardSelector = smokePage.getByTestId("dashboard-selector");
  await dashboardSelector.evaluate((element, dashboardId) => {
    if (!("value" in element) || typeof element.value !== "string") {
      throw new Error("dashboard selector has no value");
    }
    element.value = dashboardId;
    element.dispatchEvent(new Event("change", { bubbles: true }));
  }, stack.secondDashboardId);
  await expect.poll(() => dashboardSelector.evaluate((element) =>
    "value" in element ? String(element.value) : ""
  )).toBe(stack.secondDashboardId);
  const browseUnavailable = smokePage.getByTestId("browse-worker-unavailable");
  await expect(browseUnavailable).toBeVisible();
  await expect(browseUnavailable).toHaveAttribute("role", "status");
  await expect(browseUnavailable).toHaveAttribute("aria-live", "polite");
  await expect(browseUnavailable).toHaveAccessibleName(
    "Machine unavailable. This machine isn't available in the current dashboard.",
  );
  await expect(browseUnavailable).toBeFocused();

  await smokePage.setViewportSize({ width: 640, height: 320 });

  await smokePage.evaluate((workerFp) => {
    const smokeWindow = window as unknown as WorkerRouteGuardWindow;
    smokeWindow.__smoke.navigate(`/file/${workerFp}/tmp/unavailable.txt`);
  }, stack.workerFp);
  const fileUnavailable = smokePage.getByTestId("file-viewer-unavailable");
  const homeAction = smokePage.getByTestId("file-viewer-unavailable-home");
  await expect(fileUnavailable).toBeVisible();
  await expect(fileUnavailable).toHaveAttribute("role", "status");
  await expect(fileUnavailable).toHaveAttribute("aria-live", "polite");
  await expect(fileUnavailable).toHaveAccessibleName(
    "File unavailable. This file isn't available in the current dashboard.",
  );
  await expect(fileUnavailable).toBeFocused();

  const scrollState = await fileUnavailable.evaluate((element) => {
    if (!(element instanceof HTMLElement)) {
      throw new Error("file denial container is not an element");
    }
    return {
      clientHeight: element.clientHeight,
      scrollHeight: element.scrollHeight,
      overflowY: getComputedStyle(element).overflowY,
    };
  });
  expect(scrollState.overflowY).toBe("auto");
  expect(scrollState.scrollHeight).toBeGreaterThan(scrollState.clientHeight);

  await smokePage.keyboard.press("Tab");
  await expect(homeAction).toBeFocused();
  await expect(homeAction).toBeInViewport({ ratio: 1 });
});
