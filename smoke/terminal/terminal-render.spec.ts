import { test, expect } from "./fixtures.ts";
import { fixturePath, waitForStableCellFrames } from "./terminal-helpers.ts";

test("trusted keyboard input and bottom-follow behavior", async ({ smokePage, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const marker = `PW_INPUT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  await smokePage.keyboard.type("for i in $(seq 1 500); do echo BOTTOMLINE-$i; done");
  await smokePage.keyboard.press("Enter");
  await expect.poll(async () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { renderProbe(sessionId: string): { rowCount: number; atBottom: boolean } } }).__smoke;
    return smoke.renderProbe(id);
  }, sessionId)).toMatchObject({ atBottom: true });
});

test("streaming sequence repair leaves an off-bottom reader fixed", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "for i in $(seq 1 1500); do printf 'READERLINE-%04d stable-history\\n' $i; done\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { max: number } };
    }).__smoke;
    return smoke.markerScan(id, "READERLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(1500);

  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("streaming reader grid missing");
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -6000);
  const sample = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        cellGridEpoch(sessionId: string): string;
        droppedCellFrameCount(sessionId: string): number;
      };
    }).__smoke;
    const container = document.querySelector(
      `[data-testid="terminal-slot-${id}"] .wterm.cell-grid`,
    ) as HTMLElement;
    const rect = container.getBoundingClientRect();
    const row = document.elementFromPoint(rect.left + 100, rect.top + 200)?.closest(".cell-row");
    return {
      scrollTop: container.scrollTop,
      scrollHeight: container.scrollHeight,
      row: row?.textContent ?? "",
      rowOffset: row ? row.getBoundingClientRect().top - (rect.top + 200) : null,
      frames: smoke.cellFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      fullFrames: smoke.cellFullFrameCount(id),
      dropped: smoke.droppedCellFrameCount(id),
    };
  }, sessionId);
  await expect.poll(async () => (await sample()).row, { timeout: 10_000 })
    .toContain("READERLINE-");
  const before = await sample();

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        dropNextCellFrame(sessionId: string): void;
        input(sessionId: string, text: string): Promise<void>;
      };
    }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "for i in $(seq 1501 1800); do printf 'READERLINE-%04d streaming\\n' $i; sleep 0.01; done\r",
    );
  }, sessionId);
  await expect.poll(() => sample(), { timeout: 60_000 }).toMatchObject({
    fullFrames: before.fullFrames + 1,
    dropped: before.dropped + 1,
  });
  await expect.poll(async () => (await sample()).frames, { timeout: 60_000 })
    .toBeGreaterThan(before.frames + 200);

  const during = await sample();
  expect(during.gridEpoch).toBe(before.gridEpoch);
  expect(during.scrollTop).toBe(before.scrollTop);
  expect(during.scrollHeight).toBe(before.scrollHeight);
  expect(during.row).toBe(before.row);
  expect(during.rowOffset).toBe(before.rowOffset);

  await smokePage.mouse.wheel(0, 100_000);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number };
      };
    }).__smoke;
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: smoke.markerScan(id, "READERLINE-").max,
    };
  }, sessionId), { timeout: 30_000 }).toEqual({ atBottom: true, markerMax: 1800 });
});

test("alternate screen survives width and height perturbations", async ({ smokePage, stack }) => {
  test.setTimeout(240_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type(`'${fixturePath}'`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent()).toContain("CELLLINE-60");
  const result = await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runRenderStress(options: {
          sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number;
        }): Promise<{ verdict: string; fails: unknown[] }>;
      };
    }).__smoke;
    return smoke.runRenderStress({ sessionId: id, prefix: "CELLLINE-", screen: "alt", iterations: 80 });
  }, sessionId);
  expect(result).toMatchObject({ verdict: "PASS", fails: [] });
  await smokePage.keyboard.press("q");
});

// The alt case above can only assert dup/order: a full-screen app legitimately
// re-ranges its own repaint on every SIGWINCH. Main-screen history is stricter —
// `screen: "main"` additionally fails the run if the NEWEST marker stops being
// painted, i.e. live history was lost rather than scrolled out (a shorter deck
// legitimately paints fewer rows, so the oldest visible marker does move). This
// is the invariant the hand-driven render-stress procedure asserted and nothing
// else automates.
test("main screen history survives width and height perturbations", async ({ smokePage, stack }) => {
  test.setTimeout(240_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 60");
  await smokePage.keyboard.press("Enter");
  const range = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { min: number; max: number } };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return { min: scan.min, max: scan.max };
  }, sessionId);
  // Settle the range before the loop samples it as the baseline: a half-painted
  // history would hand the stress run a moving target and pass either way.
  await expect.poll(range, { timeout: 60_000 }).toEqual({ min: 1, max: 60 });
  await waitForStableCellFrames(smokePage, sessionId);
  const result = await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runRenderStress(options: {
          sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number;
        }): Promise<{ verdict: string; fails: unknown[] }>;
      };
    }).__smoke;
    return smoke.runRenderStress({ sessionId: id, prefix: "CELLLINE-", screen: "main", iterations: 48 });
  }, sessionId);
  expect(result).toMatchObject({ verdict: "PASS", fails: [] });
  // No post-loop range assertion: rows the shrink pushed into scrollback stay
  // there when the deck grows back — the canonical renderer letterboxes instead
  // of reflowing history (apps/web/README.md), so a returning min would assert
  // a behaviour Roost deliberately does not have.
});

test("two viewers preserve ordered terminal markers", async ({ smokePage, browser, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await smokePage.keyboard.type("for i in $(seq 1 120); do echo MULTIVIEW-$i; done");
  await smokePage.keyboard.press("Enter");
  const passiveContext = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  await passiveContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
  const passive = await passiveContext.newPage();
  try {
    await passive.goto(`${stack.baseUrl}/s/${sessionId}`);
    await passive.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await smokePage.setViewportSize({ width: 700 + (iteration % 2) * 50, height: 500 + (iteration % 3) * 40 });
    }
    const scan = await passive.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
    await passive.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await passive.setViewportSize({ width: 1350 + (iteration % 2) * 50, height: 820 + (iteration % 3) * 40 });
    }
    await smokePage.bringToFront();
    const primaryScan = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(primaryScan).toMatchObject({ duplicated: [], outOfOrder: 0 });
  } finally {
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passiveContext.close();
  }
});
