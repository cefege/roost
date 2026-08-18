import { test, expect } from "./fixtures.ts";
import type { RecoveryMarkerScan, RecoverySmokeApi } from "./terminal-smoke-api.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

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

    const sentinel = await smokePage.evaluate(() => {
      const key = `__roostResumeSentinel_${crypto.randomUUID().replaceAll("-", "")}`;
      const nonce = crypto.randomUUID();
      Object.defineProperty(document, key, {
        value: Object.freeze({ nonce }),
        configurable: false,
        enumerable: false,
      });
      return { key, nonce };
    });
    const before = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: {
          cellFrameCount(sessionId: string): number;
          renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
          scrollbackBackfillRequestCount(sessionId: string): number;
          syncWsGeneration(): number;
          forceHidden(on: boolean): void;
        };
      }).__smoke;
      const result = {
        frames: smoke.cellFrameCount(id),
        requests: smoke.scrollbackBackfillRequestCount(id),
        generation: smoke.syncWsGeneration(),
        ...smoke.renderProbe(id),
      };
      smoke.forceHidden(true);
      return result;
    }, sessionId);
    expect(before.atBottom).toBe(true);

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
    const recovered = await smokePage.evaluate(({ key, nonce }) => {
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
      const value = (document as unknown as Record<string, unknown>)[key] as { nonce?: string } | undefined;
      return { samples: runtime.__resumeSamples, sentinelSurvived: value?.nonce === nonce };
    }, sentinel);
    expect(recovered.sentinelSurvived).toBe(true);
    const authoritativeAt = recovered.samples.findIndex((sample) => sample.current);
    expect(authoritativeAt).toBeGreaterThanOrEqual(0);
    const authoritative = recovered.samples[authoritativeAt]!;
    expect(authoritative.top).toBeGreaterThanOrEqual(
      authoritative.height - authoritative.client - 2,
    );
    expect(authoritative.snapshotSbRows).toBe(0);
    expect(authoritative.historyRequests).toBe(before.requests);
    expect(authoritative.rowCount).toBeLessThan(100);
    expect(recovered.samples.slice(authoritativeAt).every((sample) =>
      sample.rowCount === authoritative.rowCount
      && sample.historyRequests === before.requests
    )).toBe(true);
    // The resume itself re-dialed: the park is gone, the generation advanced, and
    // nothing reloaded (the document sentinel above survived).
    const resumed = await smokePage.evaluate(() => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return { generation: smoke.syncWsGeneration(), status: smoke.syncRedialStatus() };
    });
    expect(resumed.generation).toBeGreaterThan(before.generation);
    expect(resumed.status.hiddenParked).toBe(false);

    // Backfill demand below is only meaningful on a pane the resume actually
    // brought back: a still-parked slot would swallow the scroll and time out
    // with no cause.
    const resumedProbe = await readTerminalStreamProbe(smokePage, sessionId);
    expect(resumedProbe.browser.slot).toMatchObject({
      registered: true,
      connected: true,
      in_layout: true,
      surface_active: true,
      css_visible: true,
    });
    // Demand is a genuine reader gesture, not a synthetic scroll event: the
    // renderer keeps its bottom pin for programmatic scrolls, so only a trusted
    // wheel expresses "I am reading history" and unlocks the backfill.
    const resumedBox = await smokePage.getByTestId(`terminal-slot-${sessionId}`).boundingBox();
    if (!resumedBox) throw new Error("resumed pane has no box to scroll");
    await smokePage.mouse.move(
      resumedBox.x + resumedBox.width / 2,
      resumedBox.y + resumedBox.height / 2,
    );
    await smokePage.mouse.wheel(0, -6000);
    await expect.poll(() => smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: RecoverySmokeApi }).__smoke;
      return smoke.renderProbe(id).atBottom;
    }, sessionId)).toBe(false);
    await smokePage.waitForFunction(({ id, previous }) => {
      const smoke = (window as unknown as Window & {
        __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
      }).__smoke;
      return smoke.scrollbackBackfillRequestCount(id) > previous;
    }, { id: sessionId, previous: before.requests });
    await smokePage.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { markerScan(sessionId: string, prefix: string): RecoveryMarkerScan };
      }).__smoke;
      const scan = smoke.markerScan(id, "HIDDEN-");
      return scan.duplicated.length === 0 && scan.missing === 0 && scan.outOfOrder === 0;
    }, sessionId);
    const history = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { markerScan(sessionId: string, prefix: string): RecoveryMarkerScan };
      }).__smoke;
      return smoke.markerScan(id, "HIDDEN-");
    }, sessionId);
    expect(history).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
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
