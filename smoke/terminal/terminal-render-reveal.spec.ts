import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  setRecoveryCanary,
  recoveryProbe,
} from "./terminal-helpers.ts";

// Returning to an already-open pane keeps the Sync socket and reclaims an
// authoritative snapshot ONLY when the grid moved while it was away. Hidden and
// offscreen panes must receive no cells either way.
test("a dormant pane reclaims only when its grid moved, and never re-dials Sync", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionA);

  await smokePage.goto(`${stack.baseUrl}/s/${sessionA}`);
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  await expect(slotA).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotA.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");
  await smokePage.waitForTimeout(1000);

  // Deck switch with NO output on A while it is parked. A withdraws offscreen,
  // and the return claim carries a held_cell_seq the worker can prove is still
  // current, so the reveal is a visibility flip: zero cells, no repaint.
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionB);
  await expect(smokePage.getByTestId(`tab-${sessionB}`)).toHaveAttribute("data-active", "true");
  await smokePage.waitForTimeout(1000);
  expect((await probe()).frames).toBe(beforeSwitch.frames);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");
  // Zero, not one: proving an absence needs a settle window, not a poll (a poll
  // that starts equal passes before the frame it is meant to catch could land).
  // The dwell on B was 1000 ms, past VIEWER_WITHDRAW_GRACE_MS (800), so this is
  // genuinely the unwatched path and not a claim that never stopped streaming.
  await smokePage.waitForTimeout(1000);
  expect(await probe()).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    frames: beforeSwitch.frames,
    fullFrames: beforeSwitch.fullFrames,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // Deterministic hidden pin: the page stays schedulable while lifecycle
  // handlers withdraw A. Output advances at the PTY but no cell reaches the
  // browser until visibility returns and one authoritative snapshot reclaims it.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  await smokePage.waitForTimeout(3000);
  expect(await probe()).toMatchObject({
    markerMax: 8000,
    frames: beforeHide.frames,
    fullFrames: beforeHide.fullFrames,
    wsGeneration: beforeHide.wsGeneration,
  });

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(false);
  });
  await expect.poll(async () => (await probe()).markerMax, { timeout: 60_000 }).toBe(8200);
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames + 1,
    wsGeneration: beforeHide.wsGeneration,
  });
});

test("offline producer divergence reconnects and repaints without a reload", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop transport recovery contract");
  test.setTimeout(90_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-READY-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-READY-001");
  await waitForStableCellFrames(smokePage, sessionId);

  const canary = `offline-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);

  const context = smokePage.context();
  await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.pauseSyncTransport(),
  );
  try {
    await context.setOffline(true);
    await expect.poll(() => smokePage.evaluate(() => navigator.onLine)).toBe(false);
    await stack.client.sessionsInput({
      sessionId,
      data: new TextEncoder().encode(
        "for i in $(seq 1 30); do printf 'OFFLINE-RECOVER-%03d\\n' \"$i\"; sleep 0.01; done; seq 1 48; printf 'OFFLINE-CURRENT-%03d\\n' 1\r",
      ),
    });
    await expect.poll(async () => {
      const cells = await stack.client.sessionsGetScrollbackCells({
        sessionId,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 250,
        gridEpoch: before.gridEpoch,
      });
      const text = cells.rows
        .map((row) => row.spans.map((span) => span.text || " ").join(""))
        .join("\n");
      return Math.max(0, ...Array.from(text.matchAll(/OFFLINE-RECOVER-(\d+)/g), (match) => Number(match[1])));
    }, { timeout: 30_000, intervals: [100] }).toBe(30);
    const isolated = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
      return {
        frames: smoke.cellFrameCount(id),
        max: smoke.markerScan(id, "OFFLINE-RECOVER-").max,
      };
    }, sessionId);
    expect(isolated).toEqual({ frames: before.frames, max: 0 });
  } finally {
    await context.setOffline(false);
    await smokePage.evaluate(
      () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.resumeSyncTransport(),
    );
  }

  await expect.poll(() => smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  ), { timeout: 30_000, intervals: [100] }).toBeGreaterThan(before.wsGeneration);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 30_000, intervals: [100] }).toContain("OFFLINE-CURRENT-001");

  const recovered = await recoveryProbe(smokePage, sessionId, "OFFLINE-RECOVER-");
  expect(recovered).toMatchObject({
    canary,
    atBottom: true,
    scan: { max: 0, duplicated: [], missing: 0, outOfOrder: 0 },
  });
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.lastFullFrameSbRows(id),
    sessionId,
  )).toBe(0);
  const afterReconnect = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(afterReconnect.fullFrames).toBe(before.fullFrames + 1);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-AFTER-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-AFTER-001");
  expect(await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  )).toBe(afterReconnect.wsGeneration);
});

// Bottom-follow must survive geometry changes that happen while a pane is
// parked: the box shrinks (window resize / keyboard inset / divider drag),
// nothing re-samples the bottom, and pre-noteBoxResize the pane revealed
// off-bottom with live output landing below the fold — permanently.
test("a pane revealed after the window shrank is still at the bottom", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SHRK-%g' 1 600");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("SHRK-600");

  // Park A behind a sibling, then shrink the window UNDER the parked pane.
  const siblingId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, siblingId);
  await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
  const vp = smokePage.viewportSize()!;
  // −100 px keeps the short side ≥ 600 (windowSizeClass compact boundary keys
  // on min(w,h)): the pane must SHRINK, not flip the whole app to mobile UI.
  await smokePage.setViewportSize({ width: vp.width, height: vp.height - 100 });
  await smokePage.waitForTimeout(400); // park restyle + ResizeObserver tick

  // Reveal: the FIRST sample with painted rows is already at the bottom.
  await smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__shrkSamples = samples;
    w.__shrkTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean; rowCount: number } };
    }).__smoke;
    const p = smoke.renderProbe(id);
    return p.rowCount > 0 && p.atBottom;
  }, sessionId), { timeout: 1_500, intervals: [50] }).toBe(true);
  await smokePage.waitForTimeout(1_000);
  const samples = await smokePage.evaluate(() => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    if (w.__shrkTimer !== undefined) window.clearInterval(w.__shrkTimer);
    return w.__shrkSamples ?? [];
  });
  expect(samples.length).toBeGreaterThan(5);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);
  // The newest marker is visible at the bottom of the SHRUNKEN box.
  const lastVisible = await smokePage.evaluate((id) => {
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(/SHRK-(\d+)/);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, sessionId);
  expect(lastVisible).toBe(600);
});
