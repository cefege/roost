// Covers duplicated browser-tab terminal identity rotation and final resize adoption.
// It keeps multi-document view ownership separate from ordinary render stress scenarios.
// The real smoke stack observes both browser and coordinator terminal-view state.

import { test, expect } from "./fixtures.ts";
import {
  acceptedGeometry,
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

test("duplicated tab rotates identity and adopts its final resize", async ({ smokePage, stack }) => {
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.evaluate(() => {
    (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true);
  });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    const geometry = acceptedGeometry(view);
    return view.status === "accepted"
      && view.active
      && view.view_id !== null
      && view.revision !== null
      && geometry !== null && geometry.cols > 0 && geometry.rows > 0
      && replica.baseline_ready
      && replica.expected_stream_id === view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === view.stream_id;
  }, { timeout: 30_000, intervals: [50, 100, 250] }).toBe(true);
  const primaryViewId = (await readTerminalStreamProbe(smokePage, sessionId)).browser.view.view_id; if (!primaryViewId) throw new Error("primary tab did not create a terminal view");

  const firstTabId = await smokePage.evaluate(() => sessionStorage.getItem("roost.tabId"));
  if (!firstTabId) throw new Error("primary tab did not persist roost.tabId");
  const copiedStorage = await smokePage.evaluate(() => Array.from(
    { length: sessionStorage.length },
    (_, index) => {
      const key = sessionStorage.key(index);
      return key === null ? null : [key, sessionStorage.getItem(key) ?? ""] as [string, string];
    },
  ).filter((entry): entry is [string, string] => entry !== null));
  const copied = new Map(copiedStorage);
  expect(copied.get("roost.tabId")).toBe(firstTabId);

  const duplicate = await smokePage.context().newPage();
  await duplicate.setViewportSize({ width: 1200, height: 800 });
  await duplicate.addInitScript((entries: Array<[string, string]>) => {
    for (const [key, value] of entries) sessionStorage.setItem(key, value);
  }, copiedStorage);
  try {
    await duplicate.goto(`${stack.baseUrl}/s/${sessionId}`);
    await duplicate.waitForFunction(
      () => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object",
    );
    await duplicate.evaluate(() => {
      (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } })
        .__smoke.forceVisible(true);
    });
    await expect(duplicate.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
    await expect.poll(
      () => duplicate.evaluate(() => sessionStorage.getItem("roost.tabId")),
      { timeout: 10_000, intervals: [20, 50, 100] },
    ).not.toBe(firstTabId);
    expect(await smokePage.evaluate(() => sessionStorage.getItem("roost.tabId")))
      .toBe(firstTabId);

    await expect.poll(async () => {
      const probe = await readTerminalStreamProbe(duplicate, sessionId);
      const { view, replica } = probe.browser;
      const coordinator = coordinatorTerminalViewState(probe);
      const geometry = acceptedGeometry(view);
      return view.status === "accepted"
        && view.active
        && view.view_id !== null
        && view.view_id !== primaryViewId
        && view.revision !== null
        && geometry !== null && geometry.cols > 0 && geometry.rows > 0
        && replica.baseline_ready
        && replica.expected_stream_id === view.stream_id
        && coordinator?.activeViews === 2
        && coordinator.streamId === view.stream_id;
    }, { timeout: 30_000, intervals: [50, 100, 250] }).toBe(true);
    const beforeResize = await readTerminalStreamProbe(duplicate, sessionId);
    const beforeView = beforeResize.browser.view;
    if (!beforeView.revision || !beforeView.stream_id) throw new Error("duplicate tab never established its bootstrap terminal view");
    const beforeRevision = BigInt(beforeView.revision);

    await duplicate.setViewportSize({ width: 720, height: 500 });
    await expect.poll(async () => {
      const probe = await readTerminalStreamProbe(duplicate, sessionId);
      const { view, replica } = probe.browser;
      const coordinator = coordinatorTerminalViewState(probe);
      const viewGeometry = acceptedGeometry(view);
      const beforeGeometry = acceptedGeometry(beforeView);
      return {
        dimensionsConverged: coordinator?.effective?.cols === view.effective_cols
          && coordinator.effective?.rows === view.effective_rows,
        reduced: viewGeometry !== null && beforeGeometry !== null
          && viewGeometry.cols <= beforeGeometry.cols && viewGeometry.rows <= beforeGeometry.rows
          && (viewGeometry.cols < beforeGeometry.cols || viewGeometry.rows < beforeGeometry.rows),
        acceptedBaseline: view.status === "accepted"
          && view.active
          && replica.baseline_ready
          && replica.expected_stream_id === view.stream_id,
        newer: view.revision !== null && BigInt(view.revision) > beforeRevision,
        newStream: view.stream_id !== null && view.stream_id !== beforeView.stream_id
          && coordinator?.streamId === view.stream_id,
        coordinatorViews: coordinator?.activeViews ?? 0,
      };
    }, { timeout: 30_000, intervals: [50, 100, 250] }).toEqual({
      dimensionsConverged: true,
      reduced: true,
      acceptedBaseline: true,
      newer: true,
      newStream: true,
      coordinatorViews: 2,
    });

    const finalProbe = await readTerminalStreamProbe(duplicate, sessionId);
    const finalView = finalProbe.browser.view;
    if (finalView.status !== "accepted" || !finalView.active) {
      throw new Error("duplicate tab lost its accepted final terminal view");
    }
    const marker = `DUP_RESIZE_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
    await duplicate.getByTestId(`terminal-slot-${sessionId}`).click();
    await duplicate.keyboard.type(`printf '${marker} '; stty size`);
    await duplicate.keyboard.press("Enter");
    await expect.poll(async () => duplicate.evaluate(({ id, prefix }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      const matches = [...smoke.viewportText(id).matchAll(
        new RegExp(`${prefix} (\\d+) (\\d+)`, "g"),
      )];
      const match = matches.at(-1);
      return match ? [Number(match[1]), Number(match[2])] : null;
    }, { id: sessionId, prefix: marker }), {
      timeout: 30_000,
      intervals: [50, 100, 250],
    }).toEqual([finalView.effective_rows, finalView.effective_cols]);
  } finally {
    await duplicate.evaluate(() => {
      (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } })
        .__smoke.forceVisible(false);
    }).catch(() => undefined);
    await duplicate.close();
    await smokePage.evaluate(() => {
      (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } })
        .__smoke.forceVisible(false);
    }).catch(() => undefined);
  }
});
