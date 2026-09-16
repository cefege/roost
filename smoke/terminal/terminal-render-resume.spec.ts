import { test, expect } from "./fixtures.ts";
import type {
  PaintedScrollbackProbe,
  RecoveryMarkerScan,
  RecoverySmokeApi,
} from "./terminal-smoke-api.ts";
import {
  expectPaintedRowsPreserved,
  expectPaintedScrollbackWellFormed,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";
import {
  demandInitialResumeScrollback,
  returnResumeScrollbackToBottom,
} from "./terminal-render-resume-history-demand.ts";
import {
  expectReservedIntervalPaintsWorkerRows,
  expectTransitionedInterval,
  readScrollbackLayoutEnd,
} from "./terminal-reserved-scrollback.ts";
test("long hidden deep-history resume paints the current viewport before history", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop visibility and geometry contract");
  test.setTimeout(180_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'HIDDEN-%g' 1 9000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("HIDDEN-9000");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { mode: "cell" | "byte" | "none" } };
    }).__smoke;
    return smoke.renderProbe(id).mode;
  }, sessionId)).toBe("cell");
  const { initialGrid, initialPainted, demandedRows } = await demandInitialResumeScrollback(
    smokePage,
    sessionId,
  );
  expect(demandedRows.length).toBeGreaterThan(1);
  expect(demandedRows.map((row) => row.index)).toEqual(
    Array.from({ length: demandedRows.length }, (_, offset) => demandedRows[0]!.index + offset),
  );
  await returnResumeScrollbackToBottom(smokePage, sessionId);
  const control = await smokePage.context().newPage();
  try {
    await control.goto(`${stack.baseUrl}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
    await control.waitForFunction(() =>
      typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await control.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceVisible(on: boolean): void };
      }).__smoke;
      smoke.forceVisible(true);
    });
    await control.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes("HIDDEN-9000");
    }, sessionId);
    await smokePage.bringToFront();
    const sentinel = await smokePage.evaluate((id) => {
      const key = `__roostResumeSentinel_${crypto.randomUUID().replaceAll("-", "")}`;
      const nonce = crypto.randomUUID();
      const terminalSlot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
      const grid = terminalSlot?.querySelector(".cell-grid");
      if (!terminalSlot || !grid) throw new Error("resume identity probe could not be installed");
      Object.defineProperty(document, key, {
        value: Object.freeze({ nonce, slot: terminalSlot, grid }),
        configurable: false,
        enumerable: false,
      });
      return { key, nonce };
    }, sessionId);
    const layoutEndBefore = await readScrollbackLayoutEnd(smokePage, sessionId);
    const before = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: {
          cellFrameCount(sessionId: string): number;
          renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
          scrollbackBackfillRequestCount(sessionId: string): number;
          syncWsGeneration(): number;
          forceHidden(on: boolean): void;
          paintedScrollback(sessionId: string): PaintedScrollbackProbe;
          markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
        };
      }).__smoke;
      const result = {
        frames: smoke.cellFrameCount(id),
        requests: smoke.scrollbackBackfillRequestCount(id),
        generation: smoke.syncWsGeneration(),
        painted: smoke.paintedScrollback(id),
        scan: smoke.markerScan(id, "HIDDEN-"),
        ...smoke.renderProbe(id),
      };
      smoke.forceHidden(true);
      return result;
    }, sessionId);
    expect(before.atBottom).toBe(true);
    expect(before.rowCount).toBeGreaterThan(0);
    expectPaintedScrollbackWellFormed(before.painted);
    expect(before.painted.rows.length).toBeGreaterThan(0);
    expect(before.painted.headSpacerPx + before.painted.tailGapPx).toBeGreaterThan(0);
    const beforeIndices = new Set(before.painted.rows.map((row) => row.index));
    const demandedSegment = demandedRows.filter((row) => beforeIndices.has(row.index));
    expect(demandedSegment.length).toBeGreaterThan(1);
    expect(demandedSegment.map((row) => row.index)).toEqual(
      Array.from({ length: demandedSegment.length }, (_, offset) => demandedSegment[0]!.index + offset),
    );
    expect(before.scan).toMatchObject({
      max: 9000,
      duplicated: [],
      outOfOrder: 0,
    });
    expect(before.scan.missing).toBeGreaterThan(0);
    // A VISIBLE page must heal from the capped-backoff floor with no resume
    // event and no reload. Drive the control viewer there now so the dormancy
    // window below doubles as its recovery budget (the cap is 30 s), and so the
    // divergent marker further down is delivered by a self-healed tube.
    const controlParked = await control.evaluate(() => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      const generation = smoke.syncWsGeneration();
      smoke.forceSyncMaxBackoff();
      return { generation, status: smoke.syncRedialStatus() };
    });
    expect(controlParked.status.hiddenParked).toBe(false);
    expect(controlParked.status.nextDelayMs).toBe(30_000);
    // Stay dormant beyond the retired 60 s hidden-stream grace. No cell frames
    // may reach this withdrawn viewer during the entire interval.
    await smokePage.waitForTimeout(62_000);
    const currentMarker = `CURRENT_${crypto.randomUUID().replaceAll("-", "")}`;
    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      smoke.forceSyncMaxBackoff();
    });
    // Hidden document at the same floor: it sleeps instead of dialing, which is
    // the only park production still has, and only until its next resume.
    await expect.poll(() => smokePage.evaluate(
      () => (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke
        .syncRedialStatus().hiddenParked,
    ), { timeout: 15_000, intervals: [100] }).toBe(true);
    expect(await smokePage.evaluate(
      () => (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke.syncRedialStatus(),
    )).toMatchObject({ nextDelayMs: 30_000, liveness: "none" });
    // The visible control viewer already healed itself during the dormancy.
    await expect.poll(() => control.evaluate(
      () => (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
    ), { timeout: 35_000, intervals: [250] }).toBeGreaterThan(controlParked.generation);
    expect(await control.evaluate(
      () => (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke.syncRedialStatus(),
    )).toMatchObject({ hiddenParked: false, liveness: "open" });
    await control.evaluate(async ({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { input(sessionId: string, text: string): Promise<void> };
      }).__smoke;
      await smoke.input(id, `printf '%s\\n' ${marker}\r`);
    }, { id: sessionId, marker: currentMarker });
    await control.waitForFunction(({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes(marker);
    }, { id: sessionId, marker: currentMarker });
    expect(await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { cellFrameCount(sessionId: string): number };
      }).__smoke;
      return smoke.cellFrameCount(id);
    }, sessionId)).toBe(before.frames);
    await smokePage.evaluate(({ id, marker }) => {
      type ResumeSample = {
        current: boolean;
        rowCount: number;
        top: number;
        height: number;
        client: number;
        snapshotSbRows: number;
        historyRequests: number;
      };
      const runtime = window as unknown as Window & {
        __resumeSamples: ResumeSample[];
        __resumeSampling: boolean;
      };
      const smoke = (window as unknown as Window & {
        __smoke: {
          lastFullFrameSbRows(sessionId: string): number;
          scrollbackBackfillRequestCount(sessionId: string): number;
        };
      }).__smoke;
      runtime.__resumeSamples = [];
      runtime.__resumeSampling = true;
      const sample = () => {
        if (!runtime.__resumeSampling) return;
        const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
        const container = pane?.querySelector(".wterm") as HTMLElement | null;
        if (container) {
          const box = container.getBoundingClientRect();
          let current = false;
          for (const row of container.querySelectorAll(".cell-row")) {
            const rowBox = row.getBoundingClientRect();
            if (rowBox.bottom <= box.top + 1 || rowBox.top >= box.bottom - 1) continue;
            if ((row.textContent ?? "").includes(marker)) current = true;
          }
          runtime.__resumeSamples.push({
            current,
            rowCount: container.querySelectorAll(".cell-row").length,
            top: container.scrollTop,
            height: container.scrollHeight,
            client: container.clientHeight,
            snapshotSbRows: smoke.lastFullFrameSbRows(id),
            historyRequests: smoke.scrollbackBackfillRequestCount(id),
          });
        }
        requestAnimationFrame(sample);
      };
      requestAnimationFrame(sample);
    }, { id: sessionId, marker: currentMarker });
    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceHidden(on: boolean): void };
      }).__smoke;
      smoke.forceHidden(false);
    });
    await smokePage.waitForFunction(({ id, marker }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { viewportText(sessionId: string): string };
      }).__smoke;
      return smoke.viewportText(id).includes(marker);
    }, { id: sessionId, marker: currentMarker }, { timeout: 30_000 });
    await smokePage.evaluate(async () => {
      for (let frame = 0; frame < 8; frame++) {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      }
    });
    const recovered = await smokePage.evaluate(({ key, nonce, id }) => {
      type ResumeSample = {
        current: boolean;
        rowCount: number;
        top: number;
        height: number;
        client: number;
        snapshotSbRows: number;
        historyRequests: number;
      };
      const runtime = window as unknown as Window & {
        __resumeSamples: ResumeSample[];
        __resumeSampling: boolean;
      };
      runtime.__resumeSampling = false;
      const value = (document as unknown as Record<string, unknown>)[key] as {
        nonce?: string; slot?: Element; grid?: Element;
      } | undefined;
      const terminalSlot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
      return {
        samples: runtime.__resumeSamples,
        identity: {
          document: value?.nonce === nonce,
          slot: value?.slot === terminalSlot,
          grid: value?.grid === terminalSlot?.querySelector(".cell-grid"),
        },
      };
    }, { ...sentinel, id: sessionId });
    expect(recovered.identity).toEqual({ document: true, slot: true, grid: true });
    expect(recovered.samples.every((sample) => sample.rowCount > 0)).toBe(true);
    const authoritativeAt = recovered.samples.findIndex((sample) => sample.current);
    expect(authoritativeAt).toBeGreaterThanOrEqual(0);
    const authoritative = recovered.samples[authoritativeAt]!;
    expect(authoritative.top).toBeGreaterThanOrEqual(
      authoritative.height - authoritative.client - 2,
    );
    expect(authoritative.snapshotSbRows).toBeGreaterThanOrEqual(0);
    expect(authoritative.historyRequests).toBe(before.requests);
    expect(authoritative.rowCount).toBe(before.rowCount);
    expect(recovered.samples.slice(authoritativeAt).every((sample) =>
      sample.rowCount === authoritative.rowCount
      && sample.historyRequests === before.requests
    )).toBe(true);
    const afterResume = await smokePage.evaluate((id) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      return {
        requests: smoke.scrollbackBackfillRequestCount(id),
        painted: smoke.paintedScrollback(id),
        scan: smoke.markerScan(id, "HIDDEN-"),
      };
    }, sessionId);
    expect(afterResume.requests).toBe(before.requests);
    // Painted history is only ever rows the worker sent: the interval the grid
    // scrolled past while this pane slept stays a reserved gap, holding its own
    // scroll space open until the worker's rows for it arrive.
    expect(afterResume.painted.rows.filter((row) => !beforeIndices.has(row.index))).toEqual([]);
    const transitioned = expectTransitionedInterval(
      layoutEndBefore,
      await readScrollbackLayoutEnd(smokePage, sessionId),
    );
    expectPaintedRowsPreserved({ ...before.painted, rows: demandedSegment }, afterResume.painted);
    expect(afterResume.scan.total).toBeGreaterThan(0);
    expect(afterResume.scan, JSON.stringify(afterResume.scan)).toMatchObject({
      max: 9000,
      duplicated: [],
      outOfOrder: 0,
    });
    expect(afterResume.scan.missing).toBeGreaterThan(0);
    // That reserved interval stays reachable: one explicit demand paints the
    // worker's own rows for exactly those indices.
    await expectReservedIntervalPaintsWorkerRows(smokePage, stack.client, sessionId, transitioned);
    await returnResumeScrollbackToBottom(smokePage, sessionId);
    // The resume itself re-dialed: the park is gone, the generation advanced,
    // and the document, slot, and renderer DOM above all survived.
    const resumed = await smokePage.evaluate(() => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      return { generation: smoke.syncWsGeneration(), status: smoke.syncRedialStatus() };
    });
    expect(resumed.generation).toBeGreaterThan(before.generation);
    expect(resumed.status.hiddenParked).toBe(false);

    const resumedProbe = await readTerminalStreamProbe(smokePage, sessionId);
    expect(resumedProbe.browser.slot).toMatchObject({
      registered: true,
      connected: true,
      in_layout: true,
      surface_active: true,
      css_visible: true,
    });
    // Demand a visible gap only after the resumed viewport has committed.
    await expect.poll(() => smokePage.evaluate((id) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      return smokeWindow.__smoke.renderProbe(id).atBottom;
    }, sessionId)).toBe(true);
    const resumedBox = await initialGrid.boundingBox();
    if (!resumedBox) throw new Error("resumed terminal has no scroll container");
    const priorHistory = await smokePage.evaluate(
      (id) => window.__smoke.paintedScrollback(id).rows, sessionId);
    await smokePage.evaluate((id) => {
      const container = document.querySelector<HTMLElement>(`[data-testid="terminal-slot-${id}"] .wterm`);
      if (!container) throw new Error("resumed terminal has no scroll container");
      const gap = [...container.querySelectorAll<HTMLElement>(".cell-sb-gap")]
        .find((candidate) => candidate.offsetHeight > container.clientHeight + 1);
      if (!gap) throw new Error("resumed terminal has no demandable scrollback gap");
      const gapTop = gap.getBoundingClientRect().top - container.getBoundingClientRect().top;
      container.scrollTop += gapTop + Math.min(container.clientHeight, gap.offsetHeight - container.clientHeight - 1);
    }, sessionId);
    await smokePage.mouse.move(resumedBox.x + resumedBox.width / 2, resumedBox.y + resumedBox.height / 2);
    await smokePage.mouse.wheel(0, -100);
    await smokePage.waitForFunction(({ id, requests, count }) =>
      window.__smoke.scrollbackBackfillRequestCount(id) > requests
        && window.__smoke.paintedScrollback(id).rows.length > count,
    { id: sessionId, requests: before.requests, count: priorHistory.length }, { timeout: 10_000 });
    await smokePage.evaluate(async () => {
      for (let frame = 0; frame < 8; frame++) {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      }
    });
    const history = await smokePage.evaluate((id) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const smoke = smokeWindow.__smoke;
      return {
        requests: smoke.scrollbackBackfillRequestCount(id),
        painted: smoke.paintedScrollback(id),
        scan: smoke.markerScan(id, "HIDDEN-"),
      };
    }, sessionId);
    expect(history.requests).toBeGreaterThan(before.requests);
    expectPaintedScrollbackWellFormed(history.painted);
    const newlyPaintedRows = history.painted.rows.filter((row) =>
      !priorHistory.some((previous) => previous.index === row.index),
    );
    expect(newlyPaintedRows.some((row, index) =>
      index > 0 && row.index === newlyPaintedRows[index - 1]!.index + 1)).toBe(true);
    expect(history.painted.readerAnchor).not.toBeNull();
    expect(history.scan).toMatchObject({
      max: 9000,
      duplicated: [],
      outOfOrder: 0,
    });
    expect(history.scan.missing).toBeGreaterThan(0);
  } finally {
    await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceHidden(on: boolean): void };
      }).__smoke;
      smoke.forceHidden(false);
    }).catch(() => undefined);
    await control.evaluate(() => {
      const smoke = (window as unknown as Window & {
        __smoke: { forceVisible(on: boolean): void };
      }).__smoke;
      smoke.forceVisible(false);
    }).catch(() => undefined);
    await control.close();
  }
});
