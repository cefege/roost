// Renderer qualification driver — finite multi-browser fixture verification.
// It accepts only the loopback fixture server and records each independent result.
// Fixture failures preserve screenshots and JSON before producing a nonzero exit.
// This is adapter evidence only; it cannot certify production terminal parity.

import { mkdir } from "node:fs/promises";
import { chromium, firefox, webkit, type Browser, type BrowserType, type Page } from "@playwright/test";

const outputRoot = ".cache/renderer-qualification";
const rawUrl = Bun.argv.slice(2).find((argument) => argument.startsWith("--url="))?.slice("--url=".length)
  ?? Bun.argv[Bun.argv.indexOf("--url") + 1];
if (!rawUrl) throw new Error("RendererQualificationUrlRequired");
const fixtureUrl = new URL(rawUrl);
interface HistoryRange { epoch: string; start: number; end: number; }
interface QualificationSnapshot {
  markerText: string;
  requestedRanges: HistoryRange[];
  historyFloor: number;
  historyEnd: number;
  epoch: string;
  expired: boolean;
  selection: string;
  lastRenderDurationMs: number;
}
interface QualificationProbe {
  ready: Promise<void>;
  snapshot(): QualificationSnapshot;
  refresh(): Promise<void>;
  scrollToAbsolute(absoluteRow: number): Promise<void>;
  selectMarker(marker: string): string;
  collapseSelection(): Promise<void>;
  queueFrame(cols: number): Promise<void> | void;
  evictThrough(floor: number): void;
  anchor(absoluteRow: number): void;
  stalePageThenReplace(): Promise<void>;
  destroy(): void;
}
declare global { interface Window { __rendererQualification?: QualificationProbe; } }

interface ScenarioResult {
  name: string;
  status: "pass" | "fail" | "unavailable";
  error?: string;
  observedMarkerText?: string;
  selectedText?: string;
  requestedHistoryRanges?: HistoryRange[];
  retainedRowFloor?: number;
  retainedRowEnd?: number;
  renderDurationMs?: number;
  pageRevealDurationMs?: number;
  durationMs?: number;
  screenshots: string[];
}
interface BrowserResult {
  rendererVersion: "0.5.0";
  browser: string;
  scenarios: ScenarioResult[];
  classification: "adapter-feasible" | "adapter-blocked" | "environment-incomplete";
}
const knownProductionParityGaps = [
  "text-blink",
  "worker-aware-file-links",
  "find-highlights",
  "predictions-and-remote-cursors",
  "input-mouse-and-ime",
  "screen-reader-behavior",
  "real-coordinator-worker-keeper-recovery",
] as const;

interface QualificationReport {
  browsers: BrowserResult[];
  knownProductionParityGaps: readonly string[];
}

interface ScenarioObservation {
  selectedText?: string;
  pageRevealDurationMs?: number;
}

function assertScenario(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function snapshot(page: Page): Promise<QualificationSnapshot> {
  return page.evaluate(() => {
    const probe = window.__rendererQualification;
    if (!probe) throw new Error("QualificationProbeMissing");
    return probe.snapshot();
  });
}

async function action(page: Page, name: "refresh" | "collapseSelection" | "stalePageThenReplace"): Promise<void> {
  await page.evaluate(async (actionName) => {
    const probe = window.__rendererQualification;
    if (!probe) throw new Error("QualificationProbeMissing");
    if (actionName === "refresh") return probe.refresh();
    if (actionName === "collapseSelection") return probe.collapseSelection();
    return probe.stalePageThenReplace();
  }, name);
}

async function actionWithNumber(page: Page, name: "scrollToAbsolute" | "queueFrame" | "evictThrough" | "anchor", value: number): Promise<void> {
  await page.evaluate(async ({ actionName, numericValue }) => {
    const probe = window.__rendererQualification;
    if (!probe) throw new Error("QualificationProbeMissing");
    if (actionName === "scrollToAbsolute") return probe.scrollToAbsolute(numericValue);
    if (actionName === "queueFrame") return probe.queueFrame(numericValue);
    if (actionName === "evictThrough") return probe.evictThrough(numericValue);
    return probe.anchor(numericValue);
  }, { actionName: name, numericValue: value });
}

async function openScenario(browser: Browser): Promise<Page> {
  const page = await browser.newPage({ viewport: { width: 800, height: 480 } });
  await page.goto(fixtureUrl.href, { waitUntil: "networkidle" });
  await page.evaluate(async () => {
    const probe = window.__rendererQualification;
    if (!probe) throw new Error("QualificationProbeMissing");
    await probe.ready;
  });
  return page;
}

async function recordScenario(browserName: string, browser: Browser, name: string, check: (page: Page, screenshots: string[]) => Promise<ScenarioObservation | void>): Promise<ScenarioResult> {
  const screenshots: string[] = [];
  const startedAt = performance.now();
  let page: Page | undefined;
  try {
    page = await openScenario(browser);
    const observation = await check(page, screenshots);
    const probeSnapshot = await snapshot(page);
    return {
      name,
      status: "pass",
      observedMarkerText: probeSnapshot.markerText,
      selectedText: observation?.selectedText ?? probeSnapshot.selection,
      requestedHistoryRanges: probeSnapshot.requestedRanges,
      retainedRowFloor: probeSnapshot.historyFloor,
      retainedRowEnd: probeSnapshot.historyEnd,
      renderDurationMs: probeSnapshot.lastRenderDurationMs,
      pageRevealDurationMs: observation?.pageRevealDurationMs,
      durationMs: performance.now() - startedAt,
      screenshots,
    };
  } catch (error) {
    if (page) {
      const path = `${outputRoot}/${browserName}-${name}-failure.png`;
      await page.screenshot({ path, fullPage: true }).catch(() => undefined);
      screenshots.push(path);
    }
    return { name, status: "fail", error: error instanceof Error ? error.message : String(error), durationMs: performance.now() - startedAt, screenshots };
  } finally {
    await page?.close();
  }
}

async function capture(page: Page, browserName: string, name: string, stage: string, screenshots: string[]): Promise<void> {
  const path = `${outputRoot}/${browserName}-${name}-${stage}.png`;
  screenshots.push(path);
  await page.screenshot({ path, fullPage: true });
}

async function runBrowser(browserName: string, browserType: BrowserType): Promise<BrowserResult> {
  let browser: Browser;
  try {
    browser = await browserType.launch();
  } catch (error) {
    const unavailable = ["cold-bottom", "distant-page", "selection-resize", "eviction", "stale-page", "cell-fidelity"]
      .map((name): ScenarioResult => ({ name, status: "unavailable", error: error instanceof Error ? error.message : String(error), screenshots: [] }));
    return { rendererVersion: "0.5.0", browser: browserName, scenarios: unavailable, classification: "environment-incomplete" };
  }
  try {
    const scenarios = [
      await recordScenario(browserName, browser, "cold-bottom", async (page, screenshots) => {
        const probeSnapshot = await snapshot(page);
        for (let row = 0; row < 24; row++) assertScenario(probeSnapshot.markerText.includes(`V${String(row).padStart(2, "0")}`), `ViewportMarkerMissing:V${row}`);
        const fetchedRows = probeSnapshot.requestedRanges.reduce((count, range) => count + range.end - range.start, 0);
        assertScenario(fetchedRows <= 10, "ColdBottomEagerHistory");
        assertScenario(await page.locator(".term-scrollback-row").count() <= 20, "ColdBottomDomUnbounded");
        await capture(page, browserName, "cold-bottom", "complete", screenshots);
      }),
      await recordScenario(browserName, browser, "distant-page", async (page, screenshots) => {
        await capture(page, browserName, "distant-page", "before", screenshots);
        const revealStartedAt = performance.now();
        await actionWithNumber(page, "scrollToAbsolute", 5_000);
        const probeSnapshot = await snapshot(page);
        assertScenario(probeSnapshot.markerText.includes("H05000"), "DistantMarkerMissing");
        assertScenario(probeSnapshot.requestedRanges.some((range) => range.start <= 5_000 && range.end > 5_000), "DistantRangeNotRequested");
        assertScenario(performance.now() - revealStartedAt >= 200, "DistantPageDidNotDelay");
        await capture(page, browserName, "distant-page", "after", screenshots);
        return { pageRevealDurationMs: performance.now() - revealStartedAt };
      }),
      await recordScenario(browserName, browser, "selection-resize", async (page, screenshots) => {
        await actionWithNumber(page, "scrollToAbsolute", 5_000);
        const selected = await page.evaluate(() => window.__rendererQualification?.selectMarker("H05000") ?? "");
        assertScenario(selected === "H05000", "SelectionMissing");
        await actionWithNumber(page, "queueFrame", 60);
        const held = await snapshot(page);
        assertScenario(held.selection === "H05000" && held.markerText.includes("H05000"), "SelectionHoldReplacedDom");
        await action(page, "collapseSelection");
        const released = await snapshot(page);
        const releasedFidelity = [
          released.markerText.includes(`V04 ${String.fromCodePoint(0x754c)}`),
          released.markerText.includes("V05 😀"),
          released.markerText.includes("V06 é"),
        ];
        assertScenario(releasedFidelity.every(Boolean), "FrameFidelityAfterSelection");
        await capture(page, browserName, "selection-resize", "released", screenshots);
        return { selectedText: held.selection };
      }),
      await recordScenario(browserName, browser, "eviction", async (page, screenshots) => {
        await actionWithNumber(page, "scrollToAbsolute", 5_000);
        await actionWithNumber(page, "anchor", 5_000);
        await actionWithNumber(page, "evictThrough", 500);
        await action(page, "refresh");
        let probeSnapshot = await snapshot(page);
        assertScenario(probeSnapshot.markerText.includes("H05000") && probeSnapshot.historyFloor === 500, "EvictionRealiasedAnchor");
        await actionWithNumber(page, "evictThrough", 5_001);
        await action(page, "refresh");
        probeSnapshot = await snapshot(page);
        assertScenario(probeSnapshot.expired && await page.locator("#qualification-notice").textContent() === "History expired", "EvictionExpiryMissing");
        await capture(page, browserName, "eviction", "expired", screenshots);
      }),
      await recordScenario(browserName, browser, "stale-page", async (page, screenshots) => {
        await action(page, "stalePageThenReplace");
        await actionWithNumber(page, "scrollToAbsolute", 4_000);
        const probeSnapshot = await snapshot(page);
        const hasOldEpochMarker = /(?:^|\s)H04000(?:\s|$)/.test(probeSnapshot.markerText);
        assertScenario(probeSnapshot.markerText.includes("E2H04000") && !hasOldEpochMarker, "StaleEpochRowsPainted");
        await capture(page, browserName, "stale-page", "complete", screenshots);
      }),
      await recordScenario(browserName, browser, "cell-fidelity", async (page, screenshots) => {
        const fidelity = await page.evaluate(() => {
          const rows = [...document.querySelectorAll("#qualification-grid > .term-row:not(.term-scrollback-row)")];
          return {
            text: document.querySelector("#qualification-grid")?.textContent ?? "",
            wide: document.querySelectorAll(".term-wide").length,
            links: document.querySelectorAll("a.term-link[href='https://example.com/']").length,
            cursorRow: rows.findIndex((row) => row.querySelector(".term-cursor") !== null),
            styled: document.querySelectorAll("[style*='color']").length,
          };
        });
        const fidelityMarkers = [
          fidelity.text.includes(`V04 ${String.fromCodePoint(0x754c)}`),
          fidelity.text.includes("V05 😀"),
          fidelity.text.includes("V06 é"),
        ];
        assertScenario(fidelityMarkers.every(Boolean), "GraphemeTextMissing");
        assertScenario(fidelity.wide >= 2 && fidelity.links === 2 && fidelity.cursorRow === 23 && fidelity.styled > 0, "PublishedRendererCellFidelityMissing");
        await capture(page, browserName, "cell-fidelity", "complete", screenshots);
      }),
    ];
    const classification = scenarios.some((scenario) => scenario.status === "fail") ? "adapter-blocked" : "adapter-feasible";
    return { rendererVersion: "0.5.0", browser: browserName, scenarios, classification };
  } finally {
    await browser.close();
  }
}

await mkdir(outputRoot, { recursive: true });
const results = await Promise.all([
  runBrowser("chromium", chromium),
  runBrowser("firefox", firefox),
  runBrowser("webkit", webkit),
]);
const report: QualificationReport = { browsers: results, knownProductionParityGaps };
await Bun.write(`${outputRoot}/results.json`, JSON.stringify(report, null, 2));
console.log(JSON.stringify(report, null, 2));
if (results.some((browser) => browser.classification === "adapter-blocked")) process.exitCode = 1;
