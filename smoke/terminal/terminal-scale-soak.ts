#!/usr/bin/env bun
// Opt-in 30-minute 500-session terminal qualification over the real local stack.
// Capacity and inherited resource limits fail before session allocation; workers,
// PTYs, browser contexts, and keepers are released deterministically afterwards.
// Run only with a current smoke-enabled web build: bun smoke/terminal/terminal-scale-soak.ts.

import { chromium, type Browser, type Page } from "@playwright/test";
import { setTimeout as delay } from "node:timers/promises";
import { DECK_WARM_LIMIT } from "../../apps/web/src/lib/deckWarmSet.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { waitForStableCellFrames } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import {
  assertScale,
  cleanupScaleSessions,
  closeScaleDocuments,
  createScaleDocuments,
  navigateAndPaint,
  readScaleDimensions,
  runPacedBatches,
  scaleRunId,
  sendFixtureCommand,
  spawnScaleSessions,
  waitForPaintedScaleMarker,
  waitForScaleCondition,
  type ScaleDocument,
  type ScaleSession,
  type ScaleSlot,
  type ScaleSmokeWindow,
  SCALE_ACTIVATION_PACE_MS,
  SCALE_OUTPUT_PACE_MS,
  SCALE_RENDERED_PANE_LIMIT,
  SCALE_SOAK_CONTEXTS,
  SCALE_SOAK_DOCUMENTS,
  SCALE_SOAK_PAGES_PER_CONTEXT,
  SCALE_SOAK_SESSIONS,
} from "./terminal-scale-browser.ts";
import {
  assertFleetCapacity,
  preflightSoakResources,
  type ScaleResourcePreflight,
  type ScaleWorkerCapacity,
  waitForWorkerCapacity,
} from "./terminal-scale-preflight.ts";

const QUALIFICATION_DURATION_MS = 30 * 60_000;
const SOAK_ACTIVATION_CONCURRENCY = 8;
const SOAK_ACTIVATION_PACE_MS = Math.max(SCALE_ACTIVATION_PACE_MS, 125);
const SOAK_OUTPUT_PACE_MS = Math.max(SCALE_OUTPUT_PACE_MS, 30);
const TARGET_COLS = 80;
const TARGET_ROWS = 24;
const CALIBRATION_ATTEMPTS = 6;

interface GeometrySample {
  cols: number;
  rows: number;
  gridWidth: number;
  gridHeight: number;
}

interface SoakReport {
  durationMs: number;
  sessionCount: number;
  fixtureWorkerCount: number;
  syncSocketCount: number;
  activeRenderedPanes: number;
  mountedRendererCount: number;
  mountedRendererCeiling: number;
  pulseRounds: number;
  capacities: Array<Record<string, string | number>>;
  resources: Record<string, string | null>;
}

function clampViewport(value: number, minimum: number, maximum: number): number {
  return Math.max(minimum, Math.min(maximum, Math.round(value)));
}

function environmentDurationMs(): number {
  const raw = process.env.ROOST_SCALE_SOAK_MINUTES;
  if (raw === undefined) return QUALIFICATION_DURATION_MS;
  if (!/^\d+$/.test(raw)) throw new Error("ROOST_SCALE_SOAK_MINUTES must be an integer number of minutes");
  const durationMs = Number(raw) * 60_000;
  if (!Number.isSafeInteger(durationMs) || durationMs < QUALIFICATION_DURATION_MS) {
    throw new Error("ROOST_SCALE_SOAK_MINUTES must be at least 30 for a qualified soak");
  }
  return durationMs;
}

async function readGeometrySample(page: Page, sessionId: string): Promise<GeometrySample> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    const dimensions = smokeWindow.__smoke.terminalDimensions(id);
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const grid = slot?.querySelector(".cell-grid");
    if (!(grid instanceof HTMLElement)) throw new Error("active terminal grid was unavailable for geometry calibration");
    const rect = grid.getBoundingClientRect();
    return { cols: dimensions.cols, rows: dimensions.rows, gridWidth: rect.width, gridHeight: rect.height };
  }, sessionId);
}

async function calibrateDocumentTo80x24(page: Page, sessionId: string): Promise<void> {
  for (let attempt = 0; attempt < CALIBRATION_ATTEMPTS; attempt++) {
    const sample = await readGeometrySample(page, sessionId);
    if (sample.cols === TARGET_COLS && sample.rows === TARGET_ROWS) return;
    const viewport = page.viewportSize();
    assertScale(viewport, "scale browser context omitted a configurable viewport");
    assertScale(sample.cols > 0 && sample.rows > 0 && sample.gridWidth > 0 && sample.gridHeight > 0,
      "terminal geometry calibration observed an empty grid");
    const cellWidth = sample.gridWidth / sample.cols;
    const cellHeight = sample.gridHeight / sample.rows;
    await page.setViewportSize({
      width: clampViewport(viewport.width + (TARGET_COLS - sample.cols) * cellWidth, 800, 1_600),
      height: clampViewport(viewport.height + (TARGET_ROWS - sample.rows) * cellHeight, 480, 1_200),
    });
    await delay(350);
  }
  const final = await readScaleDimensions(page, sessionId);
  throw new Error(`80x24 calibration failed at ${final.cols}x${final.rows}`);
}

async function provePtySize(page: Page, session: ScaleSession, nonce: string): Promise<void> {
  const expected = `SIZE:${nonce}:${TARGET_COLS}x${TARGET_ROWS}`;
  await sendFixtureCommand(page, session.id, encodePtyFixtureCommand({ op: "REPORT_SIZE", nonce }));
  await waitForPaintedScaleMarker(page, session.id, expected);
}

async function activateSoakSlots(
  slots: ScaleSlot[],
  sessions: readonly ScaleSession[],
  offset: number,
  calibratedPages: Set<Page>,
  runId: string,
): Promise<void> {
  await runPacedBatches(slots, SOAK_ACTIVATION_CONCURRENCY, SOAK_ACTIVATION_PACE_MS, async (slot, index) => {
    const session = sessions[(offset + index) % sessions.length]!;
    slot.session = session;
    await navigateAndPaint(slot.document.page, session.id);
    if (!calibratedPages.has(slot.document.page)) {
      await calibrateDocumentTo80x24(slot.document.page, session.id);
      calibratedPages.add(slot.document.page);
    }
    await waitForScaleCondition(`80x24 handle ${session.id}`, 15_000, async () => {
      const dimensions = await readScaleDimensions(slot.document.page, session.id);
      return dimensions.cols === TARGET_COLS && dimensions.rows === TARGET_ROWS;
    });
    await provePtySize(slot.document.page, session, `SOAK-SIZE-${runId}-${offset + index}`);
  });
}

async function pulseAllSessions(page: Page, sessions: readonly ScaleSession[], round: number): Promise<Map<string, string>> {
  const markers = new Map<string, string>();
  for (const [index, session] of sessions.entries()) {
    const marker = `SOAK-PULSE-${round}-${index}`;
    markers.set(session.id, marker);
    await sendFixtureCommand(page, session.id, encodePtyFixtureCommand({ op: "EMIT", text: marker }));
    await delay(SOAK_OUTPUT_PACE_MS);
  }
  return markers;
}

async function rotatePulseSlots(
  slots: ScaleSlot[],
  sessions: readonly ScaleSession[],
  markers: ReadonlyMap<string, string>,
): Promise<void> {
  for (let offset = 0; offset < sessions.length; offset += slots.length) {
    await runPacedBatches(slots, SOAK_ACTIVATION_CONCURRENCY, SOAK_ACTIVATION_PACE_MS, async (slot, index) => {
      const session = sessions[(offset + index) % sessions.length]!;
      const marker = markers.get(session.id);
      assertScale(marker, `pulse marker was absent for ${session.id}`);
      slot.session = session;
      await navigateAndPaint(slot.document.page, session.id, marker);
      const dimensions = await readScaleDimensions(slot.document.page, session.id);
      assertScale(dimensions.cols === TARGET_COLS && dimensions.rows === TARGET_ROWS,
        `${session.id} lost its 80x24 view handle during rotation`);
    });
  }
}

async function assertSoakDrain(slots: readonly ScaleSlot[]): Promise<{ active: number; mounted: number }> {
  let active = 0;
  let mounted = 0;
  for (const slot of slots) {
    await waitForStableCellFrames(slot.document.page, slot.session.id);
    const probe = await readTerminalStreamProbe(slot.document.page, slot.session.id);
    assertScale(probe.browser.sync.ready && probe.browser.view.active,
      `${slot.session.id} did not retain an active Sync view`);
    assertScale(probe.browser.replica.baseline_ready && !probe.browser.replica.resync_latched,
      `${slot.session.id} did not converge after paced output`);
    assertScale(JSON.stringify(probe.browser.handler_canonical) === JSON.stringify(probe.browser.dom_reconciled),
      `${slot.session.id} rendered state did not drain to its canonical watermark`);
    active += 1;
    const documentMounted = await slot.document.page.locator('[data-testid^="terminal-slot-"]').count();
    assertScale(documentMounted <= DECK_WARM_LIMIT + 1,
      `${slot.session.id} exceeded its document warm-pane bound`);
    mounted += documentMounted;
  }
  assertScale(active === SCALE_RENDERED_PANE_LIMIT,
    `active rendered panes ${active} did not equal ${SCALE_RENDERED_PANE_LIMIT}`);
  return { active, mounted };
}

async function countSyncSockets(slots: readonly ScaleSlot[]): Promise<number> {
  const socketIds = new Set<string>();
  for (const slot of slots) {
    const probe = await readTerminalStreamProbe(slot.document.page, slot.session.id);
    const socketId = probe.browser.sync.socket_id;
    assertScale(probe.browser.sync.ready && socketId,
      `${slot.session.id} did not establish a Sync socket`);
    socketIds.add(socketId);
  }
  assertScale(socketIds.size >= SCALE_SOAK_CONTEXTS,
    `only ${socketIds.size} Sync sockets established; need at least ${SCALE_SOAK_CONTEXTS}`);
  return socketIds.size;
}

function serialiseCapacity(capacity: ScaleWorkerCapacity): Record<string, string | number> {
  return {
    workerFp: capacity.workerFp,
    label: capacity.label,
    used: capacity.used,
    pending: capacity.pending,
    capacity: capacity.capacity,
    available: capacity.available,
    effectiveMemoryCeilingBytes: capacity.effectiveMemoryCeilingBytes.toString(),
    estimatedReservedBytes: capacity.estimatedReservedBytes.toString(),
    refusalCount: capacity.refusalCount.toString(),
  };
}

function serialiseResourceValue(candidate: bigint | "unlimited" | null): string | null {
  if (typeof candidate === "bigint") return candidate.toString();
  return candidate;
}

function serialiseResources(resources: ScaleResourcePreflight): Record<string, string | null> {
  return {
    requiredReservedBytes: resources.requiredReservedBytes.toString(),
    openFiles: serialiseResourceValue(resources.openFiles),
    maxProcesses: serialiseResourceValue(resources.maxProcesses),
    cgroupMemoryHigh: serialiseResourceValue(resources.cgroupMemoryHigh),
    cgroupMemoryMax: serialiseResourceValue(resources.cgroupMemoryMax),
    cgroupPidsCurrent: resources.cgroupPidsCurrent?.toString() ?? null,
    cgroupPidsMax: serialiseResourceValue(resources.cgroupPidsMax),
  };
}

export async function run500SessionSoak(options: {
  browser: Browser;
  stack: TerminalTestStack;
  durationMs: number;
}): Promise<SoakReport> {
  const { browser, stack, durationMs } = options;
  assertScale(durationMs >= QUALIFICATION_DURATION_MS, "soak duration must be at least 30 minutes");
  const resources = preflightSoakResources(SCALE_SOAK_SESSIONS);
  const sessions: ScaleSession[] = [];
  let documents: ScaleDocument[] = [];
  let completed = false;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    const capacities = [await waitForWorkerCapacity(stack, fixtureWorker)];
    assertFleetCapacity(capacities, SCALE_SOAK_SESSIONS);
    await spawnScaleSessions({
      stack,
      workers: [fixtureWorker],
      capacities,
      count: SCALE_SOAK_SESSIONS,
      runId: scaleRunId(),
      paceMs: SOAK_OUTPUT_PACE_MS,
      createdSessions: sessions,
    });
    documents = await createScaleDocuments({
      browser,
      stack,
      contexts: SCALE_SOAK_CONTEXTS,
      pagesPerContext: SCALE_SOAK_PAGES_PER_CONTEXT,
      workerFps: [fixtureWorker.workerFp],
    });
    const slots = documents.map((document, index) => ({ document, session: sessions[index]! }));
    const runId = scaleRunId();
    const syncSocketCount = await countSyncSockets(slots);
    const calibratedPages = new Set<Page>();
    for (let offset = 0; offset < sessions.length; offset += slots.length) {
      await activateSoakSlots(slots, sessions, offset, calibratedPages, runId);
    }
    assertScale(calibratedPages.size === SCALE_SOAK_DOCUMENTS,
      "not every scale document reached its calibrated 80x24 handle");
    let pulseRounds = 0;
    const deadline = Date.now() + durationMs;
    while (Date.now() < deadline) {
      const markers = await pulseAllSessions(slots[0]!.document.page, sessions, pulseRounds);
      await rotatePulseSlots(slots, sessions, markers);
      pulseRounds += 1;
    }
    const drained = await assertSoakDrain(slots);
    completed = true;
    return {
      durationMs,
      sessionCount: sessions.length,
      fixtureWorkerCount: 1,
      activeRenderedPanes: drained.active,
      syncSocketCount,
      mountedRendererCount: drained.mounted,
      mountedRendererCeiling: SCALE_SOAK_DOCUMENTS * (DECK_WARM_LIMIT + 1),
      pulseRounds,
      capacities: capacities.map(serialiseCapacity),
      resources: serialiseResources(resources),
    };
  } finally {
    await closeScaleDocuments(documents);
    await cleanupScaleSessions(stack, sessions).catch((error) => {
      if (completed) throw error;
    });
  }
}

const durationMs = environmentDurationMs();
const stack = await startTerminalTestStack();
const browser = await chromium.launch({ headless: process.env.ROOST_SCALE_HEADED !== "1" });
try {
  const report = await run500SessionSoak({ browser, stack, durationMs });
  process.stdout.write(`${JSON.stringify(report)}\n`);
} finally {
  await browser.close().catch(() => undefined);
  await stack.stop();
}
