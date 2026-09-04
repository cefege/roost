// Terminal performance probes share navigation timing and PTY fixture setup through this module.
// Probe callbacks use these routines without registering additional Playwright suites.
// Fresh-context enrollment stays coupled to navigation measurement so authorization is never bypassed.

import type { Browser, BrowserContext, Page } from "@playwright/test";
import type { SpaPhaseTimeline } from "../../apps/web/src/lib/diag.ts";
import type { SmokeApi } from "../../apps/web/src/lib/smoke.ts";
import { PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
type SmokeWindow = Window & {
  readonly __smoke: SmokeApi;
  __roostDriverBeforeNavigationEpochMs?: number;
};

// Playwright serializes callbacks without module closures. This erased binding
// types the browser global once without adding a runtime helper to those callbacks.
declare const window: SmokeWindow;

const FLOOD_LINES = 20_000;
const QUALIFY = process.env.ROOST_PERF_QUALIFY === "1";

type NavigationMeasurement = {
  driverBeforeGotoEpochMs: number;
  navigationStartEpochMs: number;
  driverToPaintMs: number;
  navigationToPaintMs: number;
  phaseTimeline: SpaPhaseTimeline;
};

function workerFolder(worker: TerminalTestWorker): string {
  return process.platform === "win32" ? worker.home.replaceAll("\\", "/") : worker.home;
}

function percentile(values: readonly number[], quantile: number): number {
  if (values.length === 0) return -1;
  const sorted = [...values].sort((left, right) => left - right);
  return sorted[Math.min(sorted.length - 1, Math.floor(quantile * sorted.length))]!;
}

async function installColdInit(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    const driverEpoch = Number(new URL(location.href).searchParams.get("__roost_driver_nav"));
    if (Number.isFinite(driverEpoch) && driverEpoch > 0) {
      window.__roostDriverBeforeNavigationEpochMs = driverEpoch;
    }
  });
}

async function measureNavigation(
  page: Page,
  baseUrl: string,
  sessionId: string,
): Promise<NavigationMeasurement> {
  const driverBeforeGotoEpochMs = Date.now();
  const url = new URL(`/s/${sessionId}`, baseUrl);
  url.searchParams.set("__roost_driver_nav", String(driverBeforeGotoEpochMs));
  await page.goto(url.href, { waitUntil: "domcontentloaded" });
  await page.waitForFunction(() => typeof window.__smoke === "object");
  const measured = await page.evaluate(async ({ id, marker, driverEpoch }) => {
    const smoke = window.__smoke;
    const proof = await smoke.waitForPaintedMarker(id, marker, 60_000);
    const phaseTimeline = smoke.phaseTimeline();
    return {
      navigationStartEpochMs: phaseTimeline.navigationStartEpochMs,
      driverToPaintMs: proof.epochMs - driverEpoch,
      navigationToPaintMs: proof.epochMs - phaseTimeline.navigationStartEpochMs,
      phaseTimeline,
    };
  }, { id: sessionId, marker: PTY_FIXTURE_READY, driverEpoch: driverBeforeGotoEpochMs });
  return { driverBeforeGotoEpochMs, ...measured };
}

async function enrollFreshNavigationContext(
  context: BrowserContext,
  stack: TerminalTestStack,
): Promise<void> {
  const token = (await stack.client.authMintBootstrap({
    kind: "browser",
    label: "roost-terminal-perf-navigation",
  })).token;
  const enrollmentPage = await context.newPage();
  try {
    await enrollmentPage.goto(`${stack.baseUrl}/#pair=${encodeURIComponent(token)}`, {
      waitUntil: "domcontentloaded",
    });
    await enrollmentPage.waitForFunction(() => location.hash === "");
    await enrollmentPage.waitForFunction((expectedDashboardId) =>
      Array.from(document.querySelectorAll<HTMLSelectElement>('[data-testid="dashboard-selector"]'))
        .some((selector) => selector.value === expectedDashboardId),
    stack.dashboardId);
  } finally {
    await enrollmentPage.close();
  }
}

async function measureFreshNavigation(
  browser: Browser,
  stack: TerminalTestStack,
  sessionId: string,
): Promise<NavigationMeasurement> {
  const context = await browser.newContext();
  await installColdInit(context);
  try {
    // A new context has a distinct device key. Enroll it explicitly before the
    // measured document deep-links into this dashboard's terminal; bypassing
    // this membership gate would turn an unauthenticated URL into access.
    await enrollFreshNavigationContext(context, stack);
    const page = await context.newPage();
    return await measureNavigation(page, stack.baseUrl, sessionId);
  } finally {
    await context.close();
  }
}

async function waitForFixtureWorker(page: Page, workerFp: string): Promise<void> {
  await page.waitForFunction((fp) => {
    const smoke = window.__smoke;
    return !!smoke.state().workers[fp];
  }, workerFp);
}

async function spawnFixtureSession(
  page: Page,
  worker: TerminalTestWorker,
): Promise<string> {
  await waitForFixtureWorker(page, worker.workerFp);
  return page.evaluate(async ({ workerFp, folder }) => {
    const smoke = window.__smoke;
    return (await smoke.spawnShell(workerFp, folder)).session_id;
  }, { workerFp: worker.workerFp, folder: workerFolder(worker) });
}

async function navigateAndProve(page: Page, sessionId: string, marker: string): Promise<void> {
  await page.evaluate(({ id, expected }) => {
    const smoke = window.__smoke;
    smoke.navigate(`/s/${id}`);
    return smoke.waitForPaintedMarker(id, expected, 60_000);
  }, { id: sessionId, expected: marker });
}


export {
  FLOOD_LINES,
  QUALIFY,
  measureFreshNavigation,
  measureNavigation,
  navigateAndProve,
  percentile,
  spawnFixtureSession,
  waitForFixtureWorker,
  workerFolder,
};
export type { NavigationMeasurement };
