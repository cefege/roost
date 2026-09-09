// Browser, session, and pacing primitives used only by terminal scale scenarios.
// Calls use the smoke-enabled real stack, while bounded batches prevent a load
// generator from manufacturing the scheduling failure it is intended to detect.
// The caller owns assertions; this module owns deterministic resource release.

import type { Browser, BrowserContext, Page } from "@playwright/test";
import { setTimeout as delay } from "node:timers/promises";
import { enrollDashboardBrowser } from "./fixtures.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import { assertFleetCapacity, type ScaleWorkerCapacity, workerFolder } from "./terminal-scale-preflight.ts";

export const SCALE_FIXTURE_READY = "ROOST_PTY_READY/1";
export const SCALE_MARKER_TIMEOUT_MS = 45_000;
export const SCALE_SPAWN_PACE_MS = 75;
export const SCALE_PAGE_ENROLL_PACE_MS = 100;
export const SCALE_ACTIVATION_PACE_MS = 100;
export const SCALE_OUTPUT_PACE_MS = 35;
export const SCALE_BATCH_SIZE = 4;
export const SCALE_RENDERED_PANE_LIMIT = 32;
export const SCALE_SOAK_SESSIONS = 500;
export const SCALE_SOAK_DOCUMENTS = 32;
export const SCALE_SOAK_CONTEXTS = 8;
export const SCALE_SOAK_PAGES_PER_CONTEXT = 4;

const DEFAULT_VIEWPORT = { width: 1_280, height: 800 };

export interface ScaleMarkerScan {
  total: number;
  unique: number;
  min: number;
  max: number;
  duplicated: number[];
  missing: number;
  outOfOrder: number;
}

export interface ScaleRetainedMarkerScan {
  markerMin: number;
  markerMax: number;
  markerMissing: number;
  markerDuplicated: number[];
  markerOutOfOrder: number;
}

export interface ScaleInputCapture {
  batches: Array<{ sessionId: string; data: number[] }>;
  droppedBatches: number;
}

export interface ScaleSmokeWindow {
  __smoke: {
    input(sessionId: string, text: string): Promise<void>;
    navigate(href: string): void;
    waitForPaintedMarker(sessionId: string, marker: string, timeoutMs?: number): Promise<unknown>;
    markerScan(sessionId: string, prefix: string): ScaleMarkerScan;
    cellFullFrameCount(sessionId: string): number;
    retainedMarkerScan(
      sessionId: string,
      prefix: string,
      pageRows?: number,
    ): Promise<ScaleRetainedMarkerScan>;
    syncWsGeneration(): number;
    dropNextTerminalWireDelta(sessionId: string): void;
    forceVisible(on: boolean): void;
    forceHidden(on: boolean): void;
    paneFocused(sessionId: string): { focused: boolean };
    resetTerminalInputCapture(): void;
    terminalInputCapture(): ScaleInputCapture;
    terminalDimensions(sessionId: string): { cols: number; rows: number };
    state(): { workers: Record<string, unknown> };
  };
}

export interface ScaleSession {
  id: string;
  worker: TerminalTestWorker;
  markerPrefix: string;
}

export interface ScaleDocument {
  page: Page;
  context: BrowserContext;
  initial: boolean;
  ownsContext: boolean;
}

export interface ScaleSlot {
  document: ScaleDocument;
  session: ScaleSession;
}

export interface ScaleFrameState {
  fullFrames: number;
  syncGeneration: number;
}

export function assertScale(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(`terminal scale qualification: ${message}`);
}

export function scaleRunId(): string {
  return crypto.randomUUID().replaceAll("-", "").slice(0, 12);
}

export async function waitForScaleCondition(
  label: string,
  timeoutMs: number,
  predicate: () => boolean | Promise<boolean>,
): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await predicate()) return;
    await delay(100);
  }
  throw new Error(`terminal scale qualification timed out waiting for ${label} after ${timeoutMs}ms`);
}

async function installScaleSmokeInit(context: BrowserContext, dashboardId: string): Promise<void> {
  await context.addInitScript((selectedDashboardId) => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    localStorage.setItem("roost.dashboardId", selectedDashboardId);
  }, dashboardId);
}

async function waitForSmokePage(page: Page, workerFps: readonly string[]): Promise<void> {
  await page.waitForFunction(() => {
    const smokeWindow = window as unknown as Partial<ScaleSmokeWindow>;
    return typeof smokeWindow.__smoke === "object";
  });
  await page.waitForFunction((expectedWorkerFps) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    const state = smokeWindow.__smoke.state();
    return expectedWorkerFps.every((workerFp) => !!state.workers[workerFp]);
  }, workerFps);
}

async function enrollScalePage(
  page: Page,
  stack: TerminalTestStack,
  workerFps: readonly string[],
): Promise<void> {
  await enrollDashboardBrowser(page, stack);
  await waitForSmokePage(page, workerFps);
  await forceScalePageVisible(page, true);
}

/** Opens exactly the requested document topology, enrolling pages one at a time. */
export async function createScaleDocuments(options: {
  browser: Browser;
  stack: TerminalTestStack;
  contexts: number;
  pagesPerContext: number;
  workerFps: readonly string[];
  initialPage?: Page;
}): Promise<ScaleDocument[]> {
  const { browser, stack, contexts, pagesPerContext, workerFps, initialPage } = options;
  assertScale(Number.isSafeInteger(contexts) && contexts > 0, "context count must be positive");
  assertScale(Number.isSafeInteger(pagesPerContext) && pagesPerContext > 0, "page count must be positive");
  const documents: ScaleDocument[] = [];
  const createdContexts: BrowserContext[] = [];
  let completed = false;
  let contextsRemaining = contexts;
  try {
    if (initialPage) {
      const initialContext = initialPage.context();
      await installScaleSmokeInit(initialContext, stack.dashboardId);
      await waitForSmokePage(initialPage, workerFps);
      await forceScalePageVisible(initialPage, true);
      documents.push({ page: initialPage, context: initialContext, initial: true, ownsContext: false });
      for (let index = 1; index < pagesPerContext; index++) {
        const page = await initialContext.newPage();
        documents.push({ page, context: initialContext, initial: false, ownsContext: false });
        await enrollScalePage(page, stack, workerFps);
        await delay(SCALE_PAGE_ENROLL_PACE_MS);
      }
      contextsRemaining -= 1;
    }
    for (let contextIndex = 0; contextIndex < contextsRemaining; contextIndex++) {
      const context = await browser.newContext({ viewport: DEFAULT_VIEWPORT });
      createdContexts.push(context);
      await installScaleSmokeInit(context, stack.dashboardId);
      for (let pageIndex = 0; pageIndex < pagesPerContext; pageIndex++) {
        const page = await context.newPage();
        documents.push({ page, context, initial: false, ownsContext: true });
        await enrollScalePage(page, stack, workerFps);
        await delay(SCALE_PAGE_ENROLL_PACE_MS);
      }
    }
    assertScale(documents.length === contexts * pagesPerContext, "browser document topology was incomplete");
    completed = true;
    return documents;
  } finally {
    if (!completed) {
      await closeScaleDocuments(documents);
      for (const context of createdContexts) await context.close().catch(() => undefined);
    }
  }
}

export async function closeScaleDocuments(documents: readonly ScaleDocument[]): Promise<void> {
  const ownedContexts = new Set<BrowserContext>();
  for (const document of documents) if (document.ownsContext) ownedContexts.add(document.context);
  for (const document of documents) {
    if (!document.ownsContext && !document.initial) await document.page.close().catch(() => undefined);
    if (document.initial) {
      await document.page.evaluate(() => {
        const smokeWindow = window as unknown as ScaleSmokeWindow;
        const smoke = smokeWindow.__smoke;
        smoke.forceVisible(false);
        smoke.forceHidden(true);
      }).catch(() => undefined);
    }
  }
  for (const context of ownedContexts) await context.close().catch(() => undefined);
}

/** Spawn sequentially against the pre-reserved per-worker capacity budget. */
export async function spawnScaleSessions(options: {
  stack: TerminalTestStack;
  workers: readonly TerminalTestWorker[];
  capacities: readonly ScaleWorkerCapacity[];
  count: number;
  runId: string;
  paceMs?: number;
  createdSessions?: ScaleSession[];
}): Promise<ScaleSession[]> {
  const { stack, workers, capacities, count, runId, paceMs = SCALE_SPAWN_PACE_MS, createdSessions } = options;
  assertFleetCapacity(capacities, count);
  const workerByFp = new Map(workers.map((worker) => [worker.workerFp, worker]));
  const budgets = capacities.map((capacity) => {
    const worker = workerByFp.get(capacity.workerFp);
    assertScale(worker, `capacity report named unknown worker ${capacity.workerFp}`);
    return { worker, remaining: capacity.available };
  });
  const sessions = createdSessions ?? [];
  let cursor = 0;
  for (let index = 0; index < count; index++) {
    let selected: { worker: TerminalTestWorker; remaining: number } | undefined;
    for (let attempt = 0; attempt < budgets.length; attempt++) {
      const candidate = budgets[(cursor + attempt) % budgets.length]!;
      if (candidate.remaining > 0) {
        selected = candidate;
        cursor = (cursor + attempt + 1) % budgets.length;
        break;
      }
    }
    assertScale(selected, `no capacity budget remained before session ${index + 1}`);
    const spawned = await stack.client.sessionsSpawn({
      workerFp: selected.worker.workerFp,
      kind: "shell",
      folder: workerFolder(selected.worker),
      cols: 80,
      rows: 24,
    });
    selected.remaining -= 1;
    sessions.push({
      id: spawned.sessionId,
      worker: selected.worker,
      markerPrefix: `SCALE-${runId}-${index.toString().padStart(3, "0")}-`,
    });
    await delay(paceMs);
  }
  return sessions;
}

export async function cleanupScaleSessions(
  stack: TerminalTestStack,
  sessions: readonly ScaleSession[],
): Promise<void> {
  const failures: string[] = [];
  for (const session of sessions) {
    await stack.client.sessionsKill({ sessionId: session.id }).catch((error) => {
      failures.push(`${session.id}: ${String(error)}`);
    });
    await delay(SCALE_OUTPUT_PACE_MS);
  }
  if (failures.length > 0) throw new Error(`scale session cleanup failed: ${failures.join("; ")}`);
}

export async function sendFixtureCommand(page: Page, sessionId: string, command: string): Promise<void> {
  await page.evaluate(async ({ id, frame }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    await smokeWindow.__smoke.input(id, frame);
  }, { id: sessionId, frame: command });
}

export async function waitForPaintedScaleMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  await page.evaluate(async ({ id, expected, timeoutMs }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    await smokeWindow.__smoke.waitForPaintedMarker(id, expected, timeoutMs);
  }, { id: sessionId, expected: marker, timeoutMs: SCALE_MARKER_TIMEOUT_MS });
}

export async function navigateAndPaint(page: Page, sessionId: string, marker = SCALE_FIXTURE_READY): Promise<void> {
  await page.evaluate(async ({ id, expected, timeoutMs }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    smokeWindow.__smoke.navigate(`/s/${id}`);
    await smokeWindow.__smoke.waitForPaintedMarker(id, expected, timeoutMs);
  }, { id: sessionId, expected: marker, timeoutMs: SCALE_MARKER_TIMEOUT_MS });
}

export async function readScaleFrameState(page: Page, sessionId: string): Promise<ScaleFrameState> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    const smoke = smokeWindow.__smoke;
    return {
      fullFrames: smoke.cellFullFrameCount(id),
      syncGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);
}

export async function readScaleMarkerScan(page: Page, sessionId: string, prefix: string): Promise<ScaleMarkerScan> {
  return page.evaluate(({ id, markerPrefix }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.markerScan(id, markerPrefix);
  }, { id: sessionId, markerPrefix: prefix });
}

export async function readScaleDimensions(page: Page, sessionId: string): Promise<{ cols: number; rows: number }> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.terminalDimensions(id);
  }, sessionId);
}

export async function runPacedBatches<T>(
  entries: readonly T[],
  concurrency: number,
  paceMs: number,
  work: (entry: T, index: number) => Promise<void>,
): Promise<void> {
  assertScale(Number.isSafeInteger(concurrency) && concurrency > 0, "batch concurrency must be positive");
  for (let offset = 0; offset < entries.length; offset += concurrency) {
    const batch = entries.slice(offset, offset + concurrency);
    await Promise.all(batch.map((entry, index) => work(entry, offset + index)));
    if (offset + batch.length < entries.length) await delay(paceMs);
  }
}

export async function forceScalePageHidden(page: Page, hidden: boolean): Promise<void> {
  await page.evaluate((on) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    smokeWindow.__smoke.forceHidden(on);
  }, hidden);
}

export async function forceScalePageVisible(page: Page, visible: boolean): Promise<void> {
  await page.evaluate((on) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    smokeWindow.__smoke.forceVisible(on);
  }, visible);
}

export async function dropNextScaleWireDelta(page: Page, sessionId: string): Promise<void> {
  await page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    smokeWindow.__smoke.dropNextTerminalWireDelta(id);
  }, sessionId);
}

export async function scalePaneFocused(page: Page, sessionId: string): Promise<boolean> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.paneFocused(id).focused;
  }, sessionId);
}

export async function resetScaleInputCapture(page: Page): Promise<void> {
  await page.evaluate(() => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    smokeWindow.__smoke.resetTerminalInputCapture();
  });
}

export async function readScaleInputCapture(page: Page): Promise<ScaleInputCapture> {
  return page.evaluate(() => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.terminalInputCapture();
  });
}
