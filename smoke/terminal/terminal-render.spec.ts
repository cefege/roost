import { test, expect } from "./fixtures.ts";
import {
  fixturePath,
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnSmokeShell,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  acceptedGeometry,
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";
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
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    "for i in $(seq 1 1500); do printf 'READERLINE-%04d stable-history\\n' $i; done\r",
  );
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.markerScan(id, "READERLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(1500);
  await inputSmokeTerminal(smokePage, sessionId, "stty -echo\r");
  await waitForStableCellFrames(smokePage, sessionId);
  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("streaming reader grid missing");
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -6000);
  const sample = () => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    const container = document.querySelector(
      `[data-testid="terminal-slot-${id}"] .wterm.cell-grid`,
    );
    if (!(container instanceof HTMLElement)) throw new Error("terminal cell grid missing");
    const rect = container.getBoundingClientRect();
    const row = document.elementFromPoint(rect.left + 100, rect.top + 200)?.closest(".cell-row");
    return {
      scrollTop: container.scrollTop,
      scrollHeight: container.scrollHeight,
      row: row?.textContent ?? "",
      rowOffset: row ? row.getBoundingClientRect().top - (rect.top + 200) : null,
      frames: smoke.cellFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      dropped: smoke.droppedCellFrameCount(id),
    };
  }, sessionId);
  await expect.poll(async () => (await sample()).row, { timeout: 10_000 })
    .toContain("READERLINE-");
  // The settled, echo-free shell makes the first post-arm accepted delivery
  // the controlled 1501 marker rather than stale prompt or input-echo output.
  const before = await sample();
  const beforeStream = await readTerminalStreamProbe(smokePage, sessionId);
  expect(beforeStream.browser.handler_canonical).toEqual(beforeStream.browser.dom_reconciled);
  const beforeSeq = beforeStream.browser.handler_canonical.seq;
  if (beforeSeq === null) throw new Error("off-bottom reader omitted its canonical sequence");
  await smokePage.evaluate(async (id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "printf 'READERLINE-1501 streaming\\n'; read _; for i in $(seq 1502 1800); do printf 'READERLINE-%04d streaming\\n' $i; sleep 0.01; done; stty echo\r",
    );
  }, sessionId);
  await expect.poll(async () => (await sample()).dropped, {
    timeout: 3_000,
    intervals: [20, 50],
  }).toBe(before.dropped + 1);
  // One renderer delivery was lost; later frames may repair it immediately from the folded browser canonical replica.
  expect(await sample()).toMatchObject({
    scrollTop: before.scrollTop,
    scrollHeight: before.scrollHeight,
    row: before.row,
    rowOffset: before.rowOffset,
  });
  await inputSmokeTerminal(smokePage, sessionId, "go\r");
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
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: smoke.markerScan(id, "READERLINE-").max,
    };
  }, sessionId), { timeout: 30_000 }).toEqual({ atBottom: true, markerMax: 1800 });
  const repaired = await readTerminalStreamProbe(smokePage, sessionId);
  const canonical = repaired.browser.handler_canonical;
  expect(repaired.browser.replica).toMatchObject({
    baseline_ready: true,
    resync_latched: false,
  });
  expect(canonical.grid_epoch).toBe(beforeStream.browser.handler_canonical.grid_epoch);
  if (canonical.seq === null) throw new Error("repaired reader omitted its canonical sequence");
  expect(canonical.seq).toBeGreaterThan(beforeSeq);
  expect(repaired.browser.presentation?.canonical).toEqual(canonical);
  expect(repaired.browser.dom_reconciled).toEqual(canonical);
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
        smaller: viewGeometry !== null && beforeGeometry !== null
          && viewGeometry.cols < beforeGeometry.cols
          && viewGeometry.rows < beforeGeometry.rows,
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
      smaller: true,
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
