import { test, expect } from "./fixtures.ts";
import { waitForStableCellFrames } from "./terminal-helpers.ts";
import type {
  PaintedScrollbackProbe,
  RecoveryMarkerScan,
} from "./terminal-smoke-api.ts";
import {
  expectPaintedRowsPreserved,
  expectPaintedScrollbackWellFormed,
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
  await waitForStableCellFrames(smokePage, sessionId);
  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("deck terminal has no scroll container");
  const priorRequests = await smokePage.evaluate((id) => window.__smoke.scrollbackBackfillRequestCount(id), sessionId);
  const previousPainted = await smokePage.evaluate((id) => window.__smoke.paintedScrollback(id), sessionId);
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -100_000);
  await smokePage.waitForFunction(({ id, prior, count }) =>
    window.__smoke.scrollbackBackfillRequestCount(id) > prior
      && window.__smoke.paintedScrollback(id).rows.length > count,
  { id: sessionId, prior: priorRequests, count: previousPainted.rows.length }, { timeout: 10_000 });
  const demandedPainted = await smokePage.evaluate((id) => window.__smoke.paintedScrollback(id), sessionId);
  expectPaintedScrollbackWellFormed(demandedPainted);
  const demandedRows = demandedPainted.rows.filter((row) =>
    !previousPainted.rows.some((prior) => prior.index === row.index),
  );
  expect(demandedRows.length).toBeGreaterThan(1);
  expect(demandedRows.map((row) => row.index)).toEqual(
    Array.from({ length: demandedRows.length }, (_, offset) => demandedRows[0]!.index + offset),
  );
  await smokePage.evaluate((id) => {
    const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm`);
    if (!(container instanceof HTMLElement)) throw new Error("deck terminal has no scroll container");
    container.scrollTop = container.scrollHeight;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
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
    const viewportRows = document.querySelector(`[data-testid="terminal-slot-${id}"] .cell-viewport`)
      ?.querySelectorAll(".cell-row").length ?? 0;
    return {
      frames: smoke.cellFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      painted: smoke.paintedScrollback(id),
      scan: smoke.markerScan(id, "SWL-"),
      ...smoke.renderProbe(id),
      viewportRows,
    };
  }, sessionId);
  expect(before.rowCount).toBeGreaterThan(0);
  expect(before.atBottom).toBe(true);
  expectPaintedScrollbackWellFormed(before.painted);
  const beforeIndices = new Set(before.painted.rows.map((row) => row.index));
  const demandedSegment = demandedRows.filter((row) => beforeIndices.has(row.index));
  expect(demandedSegment.length).toBeGreaterThan(1);
  expect(demandedSegment.map((row) => row.index)).toEqual(
    Array.from({ length: demandedSegment.length }, (_, offset) => demandedSegment[0]!.index + offset),
  );
  expect(before.scan).toMatchObject({
    max: 8000,
    duplicated: [],
    outOfOrder: 0,
  });
  expect(before.scan.missing).toBeGreaterThan(0);
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
  expect(authoritative.painted).toBe(before.rowCount + before.viewportRows);
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
  expectPaintedRowsPreserved({ ...before.painted, rows: demandedSegment }, afterReveal.painted);
  expect(afterReveal.scan.total).toBeGreaterThan(0);
  expect(afterReveal.scan).toMatchObject({
    max: 8000,
    duplicated: [],
    outOfOrder: 0,
  });
  expect(afterReveal.scan.missing).toBeGreaterThan(0);
  const revealBox = await grid.boundingBox();
  if (!revealBox) throw new Error("revealed deck terminal has no scroll container");
  await smokePage.mouse.move(revealBox.x + revealBox.width / 2, revealBox.y + revealBox.height / 2);
  await smokePage.mouse.wheel(0, -100_000);
  await expect.poll(() => smokePage.evaluate((id) =>
    window.__smoke.renderProbe(id).atBottom, sessionId)).toBe(false);
  const revisited = await smokePage.evaluate(({ id, start, end }) => ({
    painted: window.__smoke.paintedScrollback(id),
    demandedRows: window.__smoke.paintedScrollbackRange(id, start, end),
  }), {
    id: sessionId,
    start: demandedSegment[0]!.index,
    end: demandedSegment[demandedSegment.length - 1]!.index + 1,
  });
  expect(revisited.painted.readerAnchor).not.toBeNull();
  expect(revisited.demandedRows).toEqual(demandedSegment);
});

