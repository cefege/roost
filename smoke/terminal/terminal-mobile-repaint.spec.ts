// Real-stack mobile fidelity for partial scroll regions over elected WebRTC and Sync.
// A fixture PTY scrolls transcript rows while repainting a global status row in one write.
// Exact DOM rows, worker-retained history, route identity, and last-column geometry are observed.
// The mobile fixture supplies phone geometry on Chromium and a real iPhone context on WebKit.

import type { Page, TestInfo } from "@playwright/test";
import { test, expect } from "./fixtures.ts";
import { startTerminalTestStack } from "./stack.ts";
import {
  encodePtyFixtureCommand,
  PTY_FIXTURE_READY,
} from "./pty-fixture-protocol.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnPtyFixtureSession,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import {
  readPeerRoute,
  waitForDirectRoute,
  waitForSyncRoute,
} from "./terminal-peer-helpers.ts";
import { expectLastColumnVisible } from "./terminal-mobile-width-proof.ts";

const OBSERVED_UPDATES = 3;
const TOTAL_UPDATES = 12;
const SUPPORTED_PROJECTS: Record<string, true> = {
  "chromium-desktop": true,
  "webkit-iphone": true,
};
const REPAINT_TIMEOUT_MS = 30_000;
const REPAINT_INTERVALS_MS = [50, 100, 250];

type RequestedCarrier = "webrtc" | "sync";
type CarrierWorkerFixtures = { requestedCarrier: RequestedCarrier };

const carrierTest = test.extend<{}, CarrierWorkerFixtures>({
  requestedCarrier: ["webrtc", { option: true, scope: "worker" }],
  stack: [async ({ requestedCarrier }, use) => {
    const stack = await startTerminalTestStack({
      terminalPeer: {
        coordinatorEnabled: true,
        coordinatorStunUrls: [],
        workerEnabled: requestedCarrier === "webrtc",
        disableLoopbackProbe: true,
      },
    });
    try {
      await use(stack);
    } finally {
      await stack.stop();
    }
  }, { scope: "worker" }],
});

const webRtcTest = carrierTest.extend({ requestedCarrier: "webrtc" });
const syncTest = carrierTest.extend({ requestedCarrier: "sync" });


function viewportRows(page: Page, sessionId: string): Promise<string[]> {
  return page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return Array.from(slot?.querySelectorAll(".cell-viewport > .cell-row") ?? [])
      .map((row) => (row.textContent ?? "").trimEnd());
  }, sessionId);
}

async function expectExactViewport(
  page: Page,
  sessionId: string,
  expectedRows: readonly string[],
  generation: number,
): Promise<void> {
  await expect.poll(
    () => viewportRows(page, sessionId),
    {
      timeout: REPAINT_TIMEOUT_MS,
      intervals: REPAINT_INTERVALS_MS,
      message: `generation ${generation}: exact mobile viewport`,
    },
  ).toEqual(expectedRows);
  const paintedRows = await viewportRows(page, sessionId);
  const status = `STATUS-${String(generation).padStart(3, "0")}`;
  expect(paintedRows).toHaveLength(expectedRows.length);
  expect(paintedRows.at(-2), `generation ${generation}: fixed panel row`).toBe("FIXED-PANEL");
  expect(paintedRows.filter((row) => row === status)).toHaveLength(1);
  expect(
    paintedRows.filter((row) => row.startsWith("STATUS-") && row !== status),
    `generation ${generation}: stale status rows`,
  ).toEqual([]);
}

function repaintSequence(rows: number, generation: number): string {
  const next = `NEXT-${String(generation).padStart(3, "0")}`;
  const status = `STATUS-${String(generation).padStart(3, "0")}`;
  return `\x1b[1;${rows - 2}r\x1b[${rows - 2};1H\r\n\r\x1b[2K${next}`
    + `\x1b[${rows};1H\r\x1b[2K${status}`;
}

function advanceExpected(
  transcriptRows: string[],
  retainedRows: string[],
  generation: number,
): void {
  retainedRows.push(transcriptRows.shift()!);
  transcriptRows.push(`NEXT-${String(generation).padStart(3, "0")}`);
}

async function proveSelectedRoute(
  page: Page,
  sessionId: string,
  carrier: RequestedCarrier,
): Promise<void> {
  if (carrier === "webrtc") {
    await waitForDirectRoute(page, sessionId, "webrtc");
    return;
  }
  await waitForSyncRoute(page, sessionId);
}


async function attachFailureDiagnostics(
  page: Page,
  sessionId: string,
  testInfo: TestInfo,
): Promise<void> {
  const route = await readPeerRoute(page, sessionId).catch((error) => ({ error: String(error) }));
  const browser = await page.evaluate((id) => ({
    snapshot: window.__smoke.terminalBrowserSnapshot(id),
    viewport: window.__smoke.viewportText(id),
    dimensions: window.__smoke.terminalDimensions(id),
    paintedScrollback: window.__smoke.paintedScrollback(id),
  }), sessionId).catch((error) => ({ error: String(error) }));
  await testInfo.attach("mobile-repaint-diagnostics.json", {
    body: Buffer.from(JSON.stringify({ route, browser }, null, 2)),
    contentType: "application/json",
  });
  const screenshot = await page.screenshot().catch(() => null);
  if (screenshot) {
    await testInfo.attach("mobile-repaint-failure.png", {
      body: screenshot,
      contentType: "image/png",
    });
  }
}

function registerMobileRepaintScenario(
  scenarioTest: typeof carrierTest,
  requestedCarrier: RequestedCarrier,
): void {
  scenarioTest(`mobile partial-region repaint stays exact over ${requestedCarrier}`, async ({
    mobileSmokePage,
    stack,
  }, testInfo) => {
    scenarioTest.skip(
      SUPPORTED_PROJECTS[testInfo.project.name] !== true,
      "mobile repaint runs on Chromium phone geometry and macOS WebKit iPhone",
    );
    scenarioTest.setTimeout(240_000);

    const fixtureWorker = await stack.startPtyFixtureWorker();
    let sessionId: string | null = null;
    try {
      sessionId = await spawnPtyFixtureSession(mobileSmokePage, fixtureWorker);
      await navigateToSmokeSession(mobileSmokePage, sessionId);
      await expect.poll(
        () => mobileSmokePage.evaluate(
          ({ id, marker }) => window.__smoke.viewportText(id).includes(marker),
          { id: sessionId, marker: PTY_FIXTURE_READY },
        ),
        { timeout: REPAINT_TIMEOUT_MS, intervals: REPAINT_INTERVALS_MS },
      ).toBe(true);
      await proveSelectedRoute(mobileSmokePage, sessionId, requestedCarrier);
      await waitForStableCellFrames(mobileSmokePage, sessionId);

      const dimensions = await mobileSmokePage.evaluate(
        (id) => window.__smoke.terminalDimensions(id),
        sessionId,
      );
      expect(dimensions.rows, "mobile partial-region proof needs six grid rows").toBeGreaterThanOrEqual(6);
      const transcriptRows = Array.from(
        { length: dimensions.rows - 2 },
        (_, index) => `TRANSCRIPT-${String(index + 1).padStart(3, "0")}`,
      );
      const retainedRows: string[] = [];
      const initialRows = [...transcriptRows, "FIXED-PANEL", "STATUS-000"];
      const seed = "\x1b[2J" + initialRows.map(
        (text, index) => `\x1b[${index + 1};1H\r\x1b[2K${text}`,
      ).join("");
      await inputSmokeTerminal(mobileSmokePage, sessionId, encodePtyFixtureCommand({
        op: "EMIT",
        text: seed,
        newline: false,
      }));
      await expectExactViewport(mobileSmokePage, sessionId, initialRows, 0);

      for (let generation = 1; generation <= OBSERVED_UPDATES; generation++) {
        await inputSmokeTerminal(mobileSmokePage, sessionId, encodePtyFixtureCommand({
          op: "EMIT",
          text: repaintSequence(dimensions.rows, generation),
          newline: false,
        }));
        advanceExpected(transcriptRows, retainedRows, generation);
        await expectExactViewport(
          mobileSmokePage,
          sessionId,
          [...transcriptRows, "FIXED-PANEL", `STATUS-${String(generation).padStart(3, "0")}`],
          generation,
        );
      }

      let burst = "";
      for (let generation = OBSERVED_UPDATES + 1; generation <= TOTAL_UPDATES; generation++) {
        burst += repaintSequence(dimensions.rows, generation);
        advanceExpected(transcriptRows, retainedRows, generation);
      }
      await inputSmokeTerminal(mobileSmokePage, sessionId, encodePtyFixtureCommand({
        op: "EMIT",
        text: burst,
        newline: false,
      }));
      await expectExactViewport(
        mobileSmokePage,
        sessionId,
        [...transcriptRows, "FIXED-PANEL", `STATUS-${String(TOTAL_UPDATES).padStart(3, "0")}`],
        TOTAL_UPDATES,
      );
      await waitForStableCellFrames(mobileSmokePage, sessionId);
      await proveSelectedRoute(mobileSmokePage, sessionId, requestedCarrier);

      const expectedTranscriptIds = retainedRows.flatMap((row) =>
        row.startsWith("TRANSCRIPT-") ? [Number(row.slice("TRANSCRIPT-".length))] : []);
      const expectedNextIds = retainedRows.flatMap((row) =>
        row.startsWith("NEXT-") ? [Number(row.slice("NEXT-".length))] : []);
      const [retainedTranscript, retainedNext, retainedStatus] = await Promise.all([
        mobileSmokePage.evaluate(
          ({ id, prefix }) => window.__smoke.retainedMarkerScan(id, prefix),
          { id: sessionId, prefix: "TRANSCRIPT-" },
        ),
        mobileSmokePage.evaluate(
          ({ id, prefix }) => window.__smoke.retainedMarkerScan(id, prefix),
          { id: sessionId, prefix: "NEXT-" },
        ),
        mobileSmokePage.evaluate(
          ({ id, prefix }) => window.__smoke.retainedMarkerScan(id, prefix),
          { id: sessionId, prefix: "STATUS-" },
        ),
      ]);
      expect(retainedTranscript.markerIds).toEqual(expectedTranscriptIds);
      expect(retainedNext.markerIds).toEqual(expectedNextIds);
      for (const scan of [retainedTranscript, retainedNext]) {
        expect(scan.markerDuplicated).toEqual([]);
        expect(scan.markerOutOfOrder).toBe(0);
        expect(scan.rowGapCount).toBe(0);
      }
      expect(retainedStatus.markerIds, "worker history contains no status rows").toEqual([]);

      const retainedPage = await stack.client.sessionsGetScrollbackCells({
        sessionId,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 4_096,
        gridEpoch: retainedTranscript.gridEpoch,
      });
      expect(Number(retainedPage.startRow), "worker history proof reads from absolute row zero").toBe(0);
      const workerHistory = retainedPage.rows.map((row) =>
        row.spans.map((span) => span.text).join("").trimEnd());
      expect(workerHistory, "worker retained transcript stays exact and append-only").toEqual(retainedRows);
      expect(
        workerHistory.filter((row) => row === "FIXED-PANEL" || row.startsWith("STATUS-")),
        "worker retained history contains no fixed panel or status rows",
      ).toEqual([]);

      await expect.poll(async () => {
        const painted = await mobileSmokePage.evaluate(
          (id) => window.__smoke.paintedScrollback(id).rows.map((row) => row.text.trimEnd()),
          sessionId,
        );
        return painted.filter((row) => row.startsWith("TRANSCRIPT-") || row.startsWith("NEXT-"));
      }, {
        timeout: REPAINT_TIMEOUT_MS,
        intervals: REPAINT_INTERVALS_MS,
        message: "browser painted the worker-authored transcript history once and in order",
      }).toEqual(retainedRows);
      const paintedHistory = await mobileSmokePage.evaluate(
        (id) => window.__smoke.paintedScrollback(id).rows.map((row) => row.text.trimEnd()),
        sessionId,
      );
      expect(
        paintedHistory.filter((row) => row === "FIXED-PANEL" || row.startsWith("STATUS-")),
        "fixed panel and status rows never enter painted history",
      ).toEqual([]);

      const edgeMarker = `EDGE_${crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase()}`;
      const edgeColumn = dimensions.cols - edgeMarker.length + 1;
      expect(edgeColumn, "mobile grid is wider than the edge marker").toBeGreaterThan(1);
      const edgeRow = Math.max(1, Math.floor((dimensions.rows - 2) / 2));
      await inputSmokeTerminal(mobileSmokePage, sessionId, encodePtyFixtureCommand({
        op: "EMIT",
        text: `\x1b[${edgeRow};${edgeColumn}H${edgeMarker}`,
        newline: false,
      }));
      await expectLastColumnVisible(
        mobileSmokePage,
        sessionId,
        edgeMarker,
        `${requestedCarrier} mobile repaint edge`,
      );
    } catch (error) {
      if (sessionId) await attachFailureDiagnostics(mobileSmokePage, sessionId, testInfo);
      throw error;
    } finally {
      if (sessionId) {
        await inputSmokeTerminal(mobileSmokePage, sessionId, encodePtyFixtureCommand({
          op: "EMIT",
          text: "\x1b[r",
          newline: false,
        })).catch(() => undefined);
      }
    }
  });
}

registerMobileRepaintScenario(webRtcTest, "webrtc");
registerMobileRepaintScenario(syncTest, "sync");
