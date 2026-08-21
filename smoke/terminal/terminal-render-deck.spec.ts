import { test, expect } from "./fixtures.ts";
import { pressPlatformShortcut } from "./terminal-helpers.ts";
import type {
  PaintedScrollbackProbe,
  RecoveryMarkerScan,
  RecoverySmokeApi,
} from "./terminal-smoke-api.ts";
import {
  coordinatorTerminalViewState,
  expectPaintedRowsPreserved,
  expectPaintedScrollbackWellFormed,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

// A stale pane keeps compatible painted history while parked. Its fresh-stream
// full updates the live tail without blanking that DOM-owned history or
// demand-fetching rows the renderer already has.
test("deck reveal preserves painted history and lands at the live bottom instantly", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop scroll-geometry contract");
  test.setTimeout(180_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SWL-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("SWL-8000");

  const siblings: string[] = [];
  for (let index = 0; index < 5; index++) {
    const siblingId = await smokePage.evaluate(async (workerFp) => {
      const smoke = (window as unknown as Window & {
        __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
      }).__smoke;
      return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
    }, stack.workerFp);
    await smokePage.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { state(): { sessions: Record<string, unknown> } };
      }).__smoke;
      return id in smoke.state().sessions;
    }, siblingId);
    await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
      smoke.navigate(`/s/${id}`);
    }, siblingId);
    await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
    siblings.push(siblingId);
  }
  await smokePage.waitForTimeout(1200);

  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
        paintedScrollback(sessionId: string): PaintedScrollbackProbe;
        markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
      };
    }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      painted: smoke.paintedScrollback(id),
      scan: smoke.markerScan(id, "SWL-"),
      ...smoke.renderProbe(id),
    };
  }, sessionId);
  expect(before.rowCount).toBeGreaterThan(1500);
  expect(before.atBottom).toBe(true);
  expectPaintedScrollbackWellFormed(before.painted);
  expect(before.scan).toMatchObject({
    max: 8000,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "seq -f 'FRESH-%g' 1 300\r");
  }, sessionId);
  await smokePage.waitForTimeout(500);
  expect(await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFrameCount(sessionId: string): number };
    }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId)).toBe(before.frames);

  await smokePage.getByTestId(`tab-${sessionId}`).click();
  const reveal = await smokePage.evaluate(async ({ id, priorRequests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{
      visibleFresh: number;
      painted: number;
      top: number;
      height: number;
      client: number;
      snapshotSbRows: number;
      historyRequests: number;
    }> = [];
    let authoritativeAt = -1;
    for (let frame = 0; frame < 180; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const container = pane?.querySelector(".wterm") as HTMLElement | null;
      if (!container) continue;
      const box = container.getBoundingClientRect();
      let visibleFresh = -1;
      for (const row of container.querySelectorAll(".cell-row")) {
        const rowBox = row.getBoundingClientRect();
        if (rowBox.bottom <= box.top + 1 || rowBox.top >= box.bottom - 1) continue;
        const match = (row.textContent ?? "").match(/FRESH-(\d+)/);
        if (match) visibleFresh = Math.max(visibleFresh, Number(match[1]));
      }
      samples.push({
        visibleFresh,
        painted: container.querySelectorAll(".cell-row").length,
        top: container.scrollTop,
        height: container.scrollHeight,
        client: container.clientHeight,
        snapshotSbRows: smoke.lastFullFrameSbRows(id),
        historyRequests: smoke.scrollbackBackfillRequestCount(id),
      });
      if (visibleFresh === 300 && authoritativeAt < 0) authoritativeAt = samples.length - 1;
      if (authoritativeAt >= 0 && samples.length >= authoritativeAt + 8) break;
    }
    return { samples, authoritativeAt, priorRequests };
  }, { id: sessionId, priorRequests: before.requests });

  expect(reveal.authoritativeAt).toBeGreaterThanOrEqual(0);
  expect(reveal.samples.every((sample) => sample.painted > 0)).toBe(true);
  expect(reveal.samples
    .filter((sample) => sample.visibleFresh >= 0)
    .every((sample) => sample.visibleFresh === 300)).toBe(true);
  const authoritative = reveal.samples[reveal.authoritativeAt]!;
  expect(authoritative.top).toBeGreaterThanOrEqual(authoritative.height - authoritative.client - 2);
  expect(authoritative.snapshotSbRows).toBeGreaterThanOrEqual(0);
  expect(authoritative.historyRequests).toBe(reveal.priorRequests);
  expect(authoritative.painted).toBeGreaterThan(1500);
  expect(reveal.samples.slice(reveal.authoritativeAt).map((sample) => ({
    painted: sample.painted,
    historyRequests: sample.historyRequests,
  }))).toEqual(Array(reveal.samples.length - reveal.authoritativeAt).fill({
    painted: authoritative.painted,
    historyRequests: reveal.priorRequests,
  }));

  const afterReveal = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        paintedScrollback(sessionId: string): PaintedScrollbackProbe;
        markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
      };
    }).__smoke;
    return {
      painted: smoke.paintedScrollback(id),
      scan: smoke.markerScan(id, "SWL-"),
    };
  }, sessionId);
  expectPaintedRowsPreserved(before.painted, afterReveal.painted, true);
  expect(afterReveal.scan.total).toBeGreaterThan(1500);
  expect(afterReveal.scan).toMatchObject({
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });

  // A trusted wheel stays inside the compatible DOM-owned history, so fetching
  // it again would be redundant.
  const revealedBox = await smokePage.getByTestId(`terminal-slot-${sessionId}`).boundingBox();
  if (!revealedBox) throw new Error("revealed pane has no box to scroll");
  await smokePage.mouse.move(
    revealedBox.x + revealedBox.width / 2,
    revealedBox.y + revealedBox.height / 2,
  );
  await smokePage.mouse.wheel(0, -6000);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean } };
    }).__smoke;
    return smoke.renderProbe(id).atBottom;
  }, sessionId)).toBe(false);
  await smokePage.evaluate(async () => {
    for (let frame = 0; frame < 8; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    }
  });
  const afterScroll = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        paintedScrollback(sessionId: string): PaintedScrollbackProbe;
        markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      painted: smoke.paintedScrollback(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      scan: smoke.markerScan(id, "SWL-"),
    };
  }, sessionId);
  expect(afterScroll.requests).toBe(reveal.priorRequests);
  expectPaintedScrollbackWellFormed(afterScroll.painted);
  expect(afterScroll.painted.readerAnchor).not.toBeNull();
  expect(afterScroll.scan).toMatchObject({
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
});

// /file hides the terminal surface and withdraws its view, but MainPane keeps
// the deck and renderer mounted. Returning therefore needs one fresh-stream
// viewport full, not a remount or a history refetch.
test("a /file round-trip keeps the deck warm and rebaselines only for renewed membership", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop deck-persistence contract");
  const identityKey = `__roostDeckIdentity_${crypto.randomUUID().replaceAll("-", "")}`;
  const canary = `deck-${crypto.randomUUID()}`;
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'FRT-%g' 1 300");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("FRT-300");

  const initialStream = await readTerminalStreamProbe(smokePage, sessionId);
  if (!initialStream.browser.view.stream_id) throw new Error("file round-trip omitted its initial stream");
  const baseline = await smokePage.evaluate(({ id, key, value }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as HTMLElement | null;
    const terminalSlot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const grid = terminalSlot?.querySelector(".cell-grid");
    if (!deck?.parentElement || !terminalSlot || !grid) return null;
    const identity = { canary: value, deck, slot: terminalSlot, grid, samples: [] as number[], sampling: false };
    Object.defineProperty(document, key, { value: identity, configurable: false });
    return {
      fullFrames: smoke.cellFullFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      wsGeneration: smoke.syncWsGeneration(),
      painted: smoke.paintedScrollback(id),
      scan: smoke.markerScan(id, "FRT-"),
    };
  }, { id: sessionId, key: identityKey, value: canary });
  if (!baseline) throw new Error("terminal deck identity probe could not be installed");
  expectPaintedScrollbackWellFormed(baseline.painted);
  expect(baseline.scan).toMatchObject({
    total: 300,
    unique: 300,
    min: 1,
    max: 300,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });

  await smokePage.evaluate((fp) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/file/${fp}/tmp/roost-frt-missing.txt`);
  }, stack.workerFp);
  await expect.poll(() => smokePage.evaluate(({ id, key, value }) => {
    const runtime = (document as unknown as Record<string, unknown>)[key] as {
      canary: string; deck: Element; slot: Element; grid: Element;
    } | undefined;
    const deck = document.querySelector('[data-testid="terminal-deck"]');
    const terminalSlot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    return {
      canary: runtime?.canary ?? null,
      sameDeck: runtime?.deck === deck,
      sameSlot: runtime?.slot === terminalSlot,
      sameGrid: runtime?.grid === terminalSlot?.querySelector(".cell-grid"),
      hostVis: deck?.parentElement ? getComputedStyle(deck.parentElement).visibility : null,
      value,
    };
  }, { id: sessionId, key: identityKey, value: canary })).toEqual({
    canary,
    sameDeck: true,
    sameSlot: true,
    sameGrid: true,
    hostVis: "hidden",
    value: canary,
  });
  await expect.poll(async () => {
    const stream = await readTerminalStreamProbe(smokePage, sessionId);
    const coordinator = coordinatorTerminalViewState(stream);
    return {
      status: stream.browser.view.status,
      active: stream.browser.view.active,
      views: coordinator?.activeViews ?? -1,
      effective: coordinator?.effective,
    };
  }).toEqual({ status: "accepted", active: false, views: 0, effective: null });

  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(slot).not.toHaveAttribute("data-spotlit", "true");
  await smokePage.evaluate(({ id, key }) => {
    const runtime = (document as unknown as Record<string, unknown>)[key] as {
      grid: Element; samples: number[]; sampling: boolean;
    };
    runtime.sampling = true;
    const sample = () => {
      if (!runtime.sampling) return;
      runtime.samples.push(runtime.grid.querySelectorAll(".cell-row").length);
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate(`/s/${id}`);
  }, { id: sessionId, key: identityKey });
  await expect(slot).toBeVisible();
  await expect.poll(async () => {
    const stream = await readTerminalStreamProbe(smokePage, sessionId);
    const coordinator = coordinatorTerminalViewState(stream);
    return stream.browser.view.status === "accepted"
      && stream.browser.view.active
      && stream.browser.view.stream_id !== initialStream.browser.view.stream_id
      && stream.browser.replica.baseline_ready
      && stream.browser.replica.expected_stream_id === stream.browser.view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === stream.browser.view.stream_id;
  }).toBe(true);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFullFrameCount(id);
  }, sessionId)).toBe(baseline.fullFrames + 1);

  const returned = await smokePage.evaluate(({ id, key }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const smoke = smokeWindow.__smoke;
    const runtime = (document as unknown as Record<string, unknown>)[key] as {
      canary: string; deck: Element; slot: Element; grid: Element;
      samples: number[]; sampling: boolean;
    };
    runtime.sampling = false;
    const deck = document.querySelector('[data-testid="terminal-deck"]');
    const terminalSlot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    return {
      canary: runtime.canary,
      sameDeck: runtime.deck === deck,
      sameSlot: runtime.slot === terminalSlot,
      sameGrid: runtime.grid === terminalSlot?.querySelector(".cell-grid"),
      hostVis: deck?.parentElement ? getComputedStyle(deck.parentElement).visibility : null,
      samples: runtime.samples,
      fullFrames: smoke.cellFullFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      wsGeneration: smoke.syncWsGeneration(),
      atBottom: smoke.renderProbe(id).atBottom,
      current: smoke.viewportText(id).includes("FRT-300"),
      painted: smoke.paintedScrollback(id),
      scan: smoke.markerScan(id, "FRT-"),
    };
  }, { id: sessionId, key: identityKey });
  expect(returned).toMatchObject({
    canary,
    sameDeck: true,
    sameSlot: true,
    sameGrid: true,
    hostVis: "visible",
    fullFrames: baseline.fullFrames + 1,
    requests: baseline.requests,
    wsGeneration: baseline.wsGeneration,
    atBottom: true,
    current: true,
  });
  expect(returned.samples.length).toBeGreaterThan(2);
  expect(returned.samples.every((painted) => painted > 0)).toBe(true);
  expectPaintedRowsPreserved(baseline.painted, returned.painted);
  expect(returned.scan).toEqual(baseline.scan);
});
