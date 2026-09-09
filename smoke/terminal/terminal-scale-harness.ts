// The serial PR-scale scenario exercises 32 real fixture PTYs over 16 documents.
// It checks marker fidelity, local recovery, independent browser progress, and
// bounded browser mounting without replacing any terminal data-plane component.
// The opt-in soak reuses browser/session primitives from terminal-scale-browser.ts.

import type { Browser, Page } from "@playwright/test";
import { setTimeout as delay } from "node:timers/promises";
import { DECK_WARM_LIMIT } from "../../apps/web/src/lib/deckWarmSet.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { activateSlots } from "./terminal-scale-activation.ts";
import type { TerminalTestStack } from "./stack.ts";
import { waitForStableCellFrames } from "./terminal-helpers.ts";
import { coordinatorTerminalViewState, readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import {
  assertScale,
  closeScaleDocuments,
  cleanupScaleSessions,
  createScaleDocuments,
  dropNextScaleWireDelta,
  forceScalePageHidden,
  forceScalePageVisible,
  readScaleFrameState,
  readScaleInputCapture,
  readScaleMarkerScan,
  resetScaleInputCapture,
  runPacedBatches,
  scalePaneFocused,
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
  SCALE_BATCH_SIZE,
  SCALE_OUTPUT_PACE_MS,
} from "./terminal-scale-browser.ts";
import { assertFleetCapacity, waitForWorkerCapacity } from "./terminal-scale-preflight.ts";

export const PR_SCALE_SESSIONS = 32;
export const PR_SCALE_CONTEXTS = 4;
export const PR_SCALE_PAGES_PER_CONTEXT = 4;
export const PR_SCALE_DOCUMENTS = PR_SCALE_CONTEXTS * PR_SCALE_PAGES_PER_CONTEXT;
export const PR_SCALE_BURST_LINES = 128;
export const PR_SCALE_TIMEOUT_MS = 240_000;

const BLOCKED_PAGE_MS = 4_500;
const WIDE_SHORT_VIEWPORT = { width: 1_440, height: 640 };
const NARROW_TALL_VIEWPORT = { width: 900, height: 960 };

export interface PrScaleQualificationReport {
  sessionCount: number;
  documentCount: number;
  mountedRendererCeiling: number;
  mountedRendererCount: number;
  dropRepairFullFrames: number;
  blockedPageMs: number;
  capacities: Array<{
    workerFp: string;
    label: string;
    used: number;
    pending: number;
    capacity: number;
    available: number;
  }>;
}

async function verifyBurst(page: Page, session: ScaleSession): Promise<void> {
  const lastMarker = `${session.markerPrefix}${PR_SCALE_BURST_LINES}`;
  await sendFixtureCommand(page, session.id, encodePtyFixtureCommand({
    op: "FLOOD",
    prefix: session.markerPrefix,
    count: PR_SCALE_BURST_LINES,
  }));
  await waitForPaintedScaleMarker(page, session.id, lastMarker);
  const visible = await readScaleMarkerScan(page, session.id, session.markerPrefix);
  const retained = await page.evaluate(({ id, prefix }) => {
    const smokeWindow = window as unknown as ScaleSmokeWindow;
    return smokeWindow.__smoke.retainedMarkerScan(id, prefix);
  }, { id: session.id, prefix: session.markerPrefix });
  assertScale(
    (retained.markerMin === 0 || retained.markerMin === 1)
      && retained.markerMissing === 0
      && retained.markerDuplicated.length === 0
      && retained.markerOutOfOrder === 0
      && visible.min <= retained.markerMax + 1
      && visible.max === PR_SCALE_BURST_LINES
      && visible.missing === 0
      && visible.duplicated.length === 0
      && visible.outOfOrder === 0,
    `${session.id} burst integrity failed: retained=${retained.markerMin}..${retained.markerMax} visible=${visible.min}..${visible.max}`,
  );
}

async function assertDrainedSlots(slots: readonly ScaleSlot[]): Promise<number> {
  let mountedTotal = 0;
  for (const slot of slots) {
    await waitForStableCellFrames(slot.document.page, slot.session.id);
    const probe = await readTerminalStreamProbe(slot.document.page, slot.session.id);
    assertScale(probe.browser.sync.ready, `${slot.session.id} Sync was not ready after drain`);
    assertScale(probe.browser.view.active && probe.browser.replica.baseline_ready,
      `${slot.session.id} was not active with a baseline after drain`);
    assertScale(!probe.browser.replica.resync_latched,
      `${slot.session.id} retained a pending terminal resync after drain`);
    assertScale(JSON.stringify(probe.browser.handler_canonical) === JSON.stringify(probe.browser.dom_reconciled),
      `${slot.session.id} DOM had not converged to its browser replica after drain`);
    const mounted = await slot.document.page.locator('[data-testid^="terminal-slot-"]').count();
    assertScale(mounted <= DECK_WARM_LIMIT + 1,
      `${slot.session.id} mounted ${mounted} panes beyond deck warm bound ${DECK_WARM_LIMIT + 1}`);
    mountedTotal += mounted;
  }
  return mountedTotal;
}

async function proveScopedDrop(dropSlot: ScaleSlot, unaffectedSlot: ScaleSlot, runId: string): Promise<number> {
  await waitForStableCellFrames(dropSlot.document.page, dropSlot.session.id);
  await waitForStableCellFrames(unaffectedSlot.document.page, unaffectedSlot.session.id);
  const before = await readScaleFrameState(dropSlot.document.page, dropSlot.session.id);
  const unaffectedBefore = await readScaleFrameState(unaffectedSlot.document.page, unaffectedSlot.session.id);
  const dropPrefix = `SCALE-DROP-${runId}-`;
  const droppedMarker = `${dropPrefix}1`;
  const marker = `${dropPrefix}2`;
  await dropNextScaleWireDelta(dropSlot.document.page, dropSlot.session.id);
  await sendFixtureCommand(dropSlot.document.page, dropSlot.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: droppedMarker }));
  await delay(SCALE_OUTPUT_PACE_MS);
  await sendFixtureCommand(dropSlot.document.page, dropSlot.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: marker }));
  await waitForPaintedScaleMarker(dropSlot.document.page, dropSlot.session.id, marker);
  await waitForScaleCondition("same-generation scoped full repair", 20_000, async () => {
    const [current, probe] = await Promise.all([
      readScaleFrameState(dropSlot.document.page, dropSlot.session.id),
      readTerminalStreamProbe(dropSlot.document.page, dropSlot.session.id),
    ]);
    const fault = probe.browser.faults;
    return fault.wire_delta_drop_count === 1
      && fault.wire_delta_dropped_seq !== null
      && fault.wire_delta_post_drop_seq !== null
      && fault.wire_delta_dropped_seq < fault.wire_delta_post_drop_seq
      && current.fullFrames === before.fullFrames + 1
      && current.syncGeneration === before.syncGeneration;
  });
  const dropScan = await readScaleMarkerScan(dropSlot.document.page, dropSlot.session.id, dropPrefix);
  assertScale(
    dropScan.total === 2 && dropScan.unique === 2 && dropScan.min === 1 && dropScan.max === 2
      && dropScan.missing === 0 && dropScan.duplicated.length === 0 && dropScan.outOfOrder === 0,
    "scoped repair lost, duplicated, or reordered its delta markers",
  );
  const unaffectedMarker = `SCALE-UNRELATED-${runId}`;
  await sendFixtureCommand(unaffectedSlot.document.page, unaffectedSlot.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: unaffectedMarker }));
  await waitForPaintedScaleMarker(unaffectedSlot.document.page, unaffectedSlot.session.id, unaffectedMarker);
  const unaffectedAfter = await readScaleFrameState(unaffectedSlot.document.page, unaffectedSlot.session.id);
  assertScale(unaffectedAfter.fullFrames === unaffectedBefore.fullFrames,
    "a one-session dropped delta forced an unrelated full repair");
  return (await readScaleFrameState(dropSlot.document.page, dropSlot.session.id)).fullFrames - before.fullFrames;
}

async function proveBlockedAndHiddenPages(slots: readonly ScaleSlot[], runId: string): Promise<void> {
  const blocked = slots[0]!;
  const healthy = slots[4]!;
  const hidden = slots[2]!;
  const blockedMarker = `SCALE-BLOCKED-${runId}`;
  await sendFixtureCommand(blocked.document.page, blocked.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: blockedMarker, delayMs: 500 }));
  await blocked.document.page.evaluate((durationMs) => {
    setTimeout(() => {
      document.documentElement.dataset.scaleBlockState = "blocking";
      const deadline = performance.now() + durationMs;
      while (performance.now() < deadline) undefined;
      document.documentElement.dataset.scaleBlockState = "finished";
    }, 0);
  }, BLOCKED_PAGE_MS);
  await delay(100);
  const healthyMarker = `SCALE-HEALTHY-${runId}`;
  const healthyStartedAt = Date.now();
  await sendFixtureCommand(healthy.document.page, healthy.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: healthyMarker }));
  await waitForPaintedScaleMarker(healthy.document.page, healthy.session.id, healthyMarker);
  assertScale(Date.now() - healthyStartedAt < BLOCKED_PAGE_MS - 100,
    "an unaffected document did not paint while its peer was blocked");
  await blocked.document.page.waitForFunction(() => document.documentElement.dataset.scaleBlockState === "finished", {
    timeout: BLOCKED_PAGE_MS + 10_000,
  });
  await waitForPaintedScaleMarker(blocked.document.page, blocked.session.id, blockedMarker);

  await forceScalePageHidden(hidden.document.page, true);
  await waitForScaleCondition("hidden terminal view", 15_000, async () => {
    const probe = await readTerminalStreamProbe(hidden.document.page, hidden.session.id);
    return !probe.browser.view.active && !probe.browser.visibility.page_visible;
  });
  const hiddenMarker = `SCALE-RESUME-${runId}`;
  await sendFixtureCommand(hidden.document.page, hidden.session.id,
    encodePtyFixtureCommand({ op: "EMIT", text: hiddenMarker }));
  await forceScalePageHidden(hidden.document.page, false);
  await forceScalePageVisible(hidden.document.page, true);
  await waitForPaintedScaleMarker(hidden.document.page, hidden.session.id, hiddenMarker);
}

async function proveCrossedGeometryAndInput(slots: ScaleSlot[], sessions: readonly ScaleSession[], runId: string): Promise<void> {
  const wide = slots[0]!;
  const narrow = slots[1]!;
  await wide.document.page.setViewportSize(WIDE_SHORT_VIEWPORT);
  await narrow.document.page.setViewportSize(NARROW_TALL_VIEWPORT);
  await activateSlots([wide, narrow], [sessions[0]!], 0);
  await waitForScaleCondition("crossed SCD geometry", 20_000, async () => {
    const [left, right] = await Promise.all([
      readTerminalStreamProbe(wide.document.page, wide.session.id),
      readTerminalStreamProbe(narrow.document.page, narrow.session.id),
    ]);
    const leftControl = coordinatorTerminalViewState(left);
    const rightControl = coordinatorTerminalViewState(right);
    return left.browser.view.active && right.browser.view.active
      && leftControl?.activeViews === 2
      && rightControl?.activeViews === 2
      && left.browser.view.stream_id === right.browser.view.stream_id
      && left.browser.view.effective_cols === right.browser.view.effective_cols
      && left.browser.view.effective_rows === right.browser.view.effective_rows
      && (left.browser.view.effective_cols ?? 0) > 0
      && (left.browser.view.effective_rows ?? 0) > 0;
  });
  await activateSlots(slots.slice(0, 4), sessions, 0);
  for (const [index, slot] of slots.slice(0, 4).entries()) {
    await slot.document.page.getByTestId(`terminal-slot-${slot.session.id}`).click();
    await waitForScaleCondition(`focused input ${index}`, 10_000,
      () => scalePaneFocused(slot.document.page, slot.session.id));
    const nonce = `SCALE-INPUT-${runId}-${index}`;
    await sendFixtureCommand(slot.document.page, slot.session.id,
      encodePtyFixtureCommand({ op: "ARM_KEY", nonce }));
    await waitForPaintedScaleMarker(slot.document.page, slot.session.id, `ARMED:${nonce}`);
    await resetScaleInputCapture(slot.document.page);
  }
  await Promise.all(slots.slice(0, 4).map((slot) => slot.document.page.keyboard.press("x")));
  for (const [index, slot] of slots.slice(0, 4).entries()) {
    const nonce = `SCALE-INPUT-${runId}-${index}`;
    await waitForPaintedScaleMarker(slot.document.page, slot.session.id, `ACK:${nonce}`);
    const capture = await readScaleInputCapture(slot.document.page);
    assertScale(capture.droppedBatches === 0 && capture.batches.length > 0,
      `input capture ${index} did not retain its addressed batch`);
    assertScale(capture.batches.every((batch) => batch.sessionId === slot.session.id),
      `input capture ${index} included another PTY session`);
  }
}

export async function runPrScaleQualification(options: {
  browser: Browser;
  stack: TerminalTestStack;
  initialPage: Page;
}): Promise<PrScaleQualificationReport> {
  const { browser, stack, initialPage } = options;
  const runId = scaleRunId();
  const sessions: ScaleSession[] = [];
  let documents: ScaleDocument[] = [];
  let completed = false;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker();
    const capacities = [await waitForWorkerCapacity(stack, fixtureWorker)];
    assertFleetCapacity(capacities, PR_SCALE_SESSIONS);
    await spawnScaleSessions({
      stack,
      workers: [fixtureWorker],
      capacities,
      count: PR_SCALE_SESSIONS,
      runId,
      createdSessions: sessions,
    });
    documents = await createScaleDocuments({
      browser,
      stack,
      contexts: PR_SCALE_CONTEXTS,
      pagesPerContext: PR_SCALE_PAGES_PER_CONTEXT,
      workerFps: [fixtureWorker.workerFp],
      initialPage,
    });
    const slots = documents.map((document, index) => ({ document, session: sessions[index]! }));
    await activateSlots(slots, sessions, 0);
    await runPacedBatches(slots, SCALE_BATCH_SIZE, SCALE_ACTIVATION_PACE_MS,
      (slot) => verifyBurst(slot.document.page, slot.session));
    await activateSlots(slots, sessions, PR_SCALE_DOCUMENTS);
    await runPacedBatches(slots, SCALE_BATCH_SIZE, SCALE_ACTIVATION_PACE_MS,
      (slot) => verifyBurst(slot.document.page, slot.session));
    await activateSlots(slots.slice(0, 2), sessions, 0);
    const dropRepairFullFrames = await proveScopedDrop(slots[0]!, slots[1]!, runId);
    await proveBlockedAndHiddenPages(slots, runId);
    await proveCrossedGeometryAndInput(slots, sessions, runId);
    const mountedRendererCount = await assertDrainedSlots(slots);
    assertScale(mountedRendererCount <= PR_SCALE_DOCUMENTS * (DECK_WARM_LIMIT + 1),
      "aggregate mounted renderer count exceeded the per-document deck bound");
    completed = true;
    return {
      sessionCount: sessions.length,
      documentCount: documents.length,
      mountedRendererCeiling: PR_SCALE_DOCUMENTS * (DECK_WARM_LIMIT + 1),
      mountedRendererCount,
      dropRepairFullFrames,
      blockedPageMs: BLOCKED_PAGE_MS,
      capacities: capacities.map((capacity) => ({
        workerFp: capacity.workerFp,
        label: capacity.label,
        used: capacity.used,
        pending: capacity.pending,
        capacity: capacity.capacity,
        available: capacity.available,
      })),
    };
  } finally {
    await closeScaleDocuments(documents);
    await cleanupScaleSessions(stack, sessions).catch((error) => {
      if (completed) throw error;
    });
  }
}
