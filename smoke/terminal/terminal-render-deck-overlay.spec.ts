// Terminal deck overlay smoke tests own route transitions that withdraw and restore
// an existing terminal view. The shared fixture provides the real coord, worker,
// keeper, browser, and terminal renderer.
import { test, expect } from "./fixtures.ts";
import { pressPlatformShortcut } from "./terminal-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  coordinatorTerminalViewState,
  expectPaintedRowsPreserved,
  expectPaintedScrollbackWellFormed,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";

test("a /file and /search round-trip keeps the deck warm", async ({ smokePage, stack }, testInfo) => {
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
  await smokePage.keyboard.type("seq -f 'FRT-%g' 1 200");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("FRT-200");

  const initialStream = await readTerminalStreamProbe(smokePage, sessionId);
  if (!initialStream.browser.view.stream_id) throw new Error("overlay round-trip omitted its initial stream");
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
    total: 200,
    unique: 200,
    min: 1,
    max: 200,
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
  await smokePage.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.navigate("/search?q=%2Ftmp");
  });
  const searchResult = smokePage.getByTestId(`global-search-result-${sessionId}`);
  await expect(searchResult).toBeVisible();

  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(slot).not.toHaveAttribute("data-spotlit", "true");
  await smokePage.evaluate(({ key }) => {
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
  }, { key: identityKey });
  await searchResult.click();
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
      current: smoke.viewportText(id).includes("FRT-200"),
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
