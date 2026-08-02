// Terminal zoom is a RESIZE, so it belongs to the history-corruption class:
// changing --term-font-size changes the measured cell box, which changes
// cols/rows for a fixed pane, which re-claims the viewport and rebuilds the
// worker's grid. This drives ten real zoom steps through the actual keyboard
// path and asserts the painted history never duplicates or reorders — the same
// invariants the resize-perturbation test asserts, for the new trigger.

import { test, expect } from "./fixtures.ts";

type ZoomSmoke = {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  input(sessionId: string, text: string): Promise<void>;
  markerScan(sessionId: string, prefix: string): {
    max: number; duplicated: number[]; outOfOrder: number;
  };
  renderProbe(sessionId: string): { rowCount: number; nonEmptyRows: number };
};

const LINES = 400;

test("terminal zoom re-sizes the grid without mangling history", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  test.setTimeout(180_000);

  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: ZoomSmoke }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();

  await smokePage.evaluate(async ({ id, lines }) => {
    const smoke = (window as unknown as Window & { __smoke: ZoomSmoke }).__smoke;
    await smoke.input(id, `seq -f 'ZOOMLINE-%g' 1 ${lines}\r`);
  }, { id: sessionId, lines: LINES });
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: ZoomSmoke }).__smoke;
    return smoke.markerScan(id, "ZOOMLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(LINES);

  const fontSize = () => smokePage.evaluate(() =>
    getComputedStyle(document.documentElement).getPropertyValue("--term-font-size").trim());
  const scan = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: ZoomSmoke }).__smoke;
    return { ...smoke.markerScan(id, "ZOOMLINE-"), rows: smoke.renderProbe(id).rowCount };
  }, sessionId);

  expect(await fontSize()).toBe("14px");
  const baseline = await scan();

  // Ten steps: out, out, out, out, in, in, in, in, in, reset. Each is a real PTY
  // resize round trip, and the claim is debounced, so settle between steps.
  const steps = ["Control+Minus", "Control+Minus", "Control+Minus", "Control+Minus",
    "Control+Equal", "Control+Equal", "Control+Equal", "Control+Equal", "Control+Equal"];
  for (const key of steps) {
    await smokePage.keyboard.press(key);
    await smokePage.waitForTimeout(400);
    const s = await scan();
    expect(s.duplicated).toEqual([]);
    expect(s.outOfOrder).toBe(0);
  }
  // Four steps down then five up nets +1 from the 14px default.
  expect(await fontSize()).toBe("15px");
  const zoomedIn = await scan();
  // A larger cell box means fewer rows fit — the claim really did change the grid.
  expect(zoomedIn.rows).toBeLessThan(baseline.rows);

  await smokePage.keyboard.press("Control+0");
  await smokePage.waitForTimeout(600);
  expect(await fontSize()).toBe("14px");
  const reset = await scan();
  expect(reset.duplicated).toEqual([]);
  expect(reset.outOfOrder).toBe(0);
  expect(reset.max).toBe(LINES);

  // The preference is per device, so it survives a reload and the pane's first
  // claim uses it — no measure-at-14px-then-re-claim flicker.
  await smokePage.keyboard.press("Control+Equal");
  await smokePage.waitForTimeout(400);
  expect(await fontSize()).toBe("15px");
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  expect(await fontSize()).toBe("15px");
});
