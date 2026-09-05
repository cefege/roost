// Shared assertions for terminal stream, viewport, and recovery smoke probes.
// Real-stack Playwright scenarios call these helpers to compare browser,
// coordinator, and worker state without duplicating null and sequence guards.

import { expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type {
  PaintedScrollbackProbe,
  RecoverySmokeApi,
} from "./terminal-smoke-api.ts";

export async function readTerminalStreamProbe(page: Page, sessionId: string): Promise<TerminalStreamProbe> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalStreamProbe(id);
  }, sessionId);
}

export async function waitForSettledTerminalRowsBelow(
  page: Page,
  sessionId: string,
  rowCeiling: number,
): Promise<number> {
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(page, sessionId)).browser;
    const domRows = browser.presentation?.rows.dom ?? null;
    const canonicalRows = browser.presentation?.rows.canonical ?? null;
    const effectiveRows = acceptedGeometry(browser.view)?.rows ?? null;
    return domRows !== null
      && browser.view.active
      && domRows < rowCeiling
      && canonicalRows === domRows
      && effectiveRows === domRows
      && browser.replica.baseline_ready;
  }).toBe(true);
  const settledRows = (await readTerminalStreamProbe(page, sessionId)).browser.presentation?.rows.dom;
  if (settledRows === undefined) throw new Error("settled presentation omitted DOM rows");
  return settledRows;
}

export async function waitForCanonicalAdvance(
  page: Page,
  sessionId: string,
  before: TerminalStreamProbe,
): Promise<TerminalStreamProbe> {
  const beforeWireSeq = before.browser.wire_received.seq;
  const beforeCanonicalSeq = before.browser.handler_canonical.seq;
  if (beforeWireSeq === null || beforeCanonicalSeq === null) {
    throw new Error("terminal stream probe omitted its baseline browser sequence");
  }
  const floor = Math.max(beforeWireSeq, beforeCanonicalSeq);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    return Math.min(
      probe.browser.wire_received.seq ?? -1,
      probe.browser.handler_canonical.seq ?? -1,
    );
  }, { timeout: 10_000, intervals: [50] }).toBeGreaterThan(floor);
  return readTerminalStreamProbe(page, sessionId);
}

export function expectCanonicalAdvanceHeld(
  before: TerminalStreamProbe,
  pending: TerminalStreamProbe,
  options: {
    epoch?: "same" | "changed";
    readerReason?: "find" | "selection" | "touch" | "wheel";
    selectionHold?: boolean;
  } = {},
): void {
  const beforeWire = before.browser.wire_received;
  const beforeCanonical = before.browser.handler_canonical;
  const beforeReconciled = before.browser.dom_reconciled;
  const wire = pending.browser.wire_received;
  const canonical = pending.browser.handler_canonical;
  const reconciled = pending.browser.dom_reconciled;
  if (beforeWire.seq === null || beforeWire.grid_epoch === null
    || beforeCanonical.seq === null || beforeCanonical.grid_epoch === null
    || beforeReconciled.seq === null || beforeReconciled.grid_epoch === null
    || wire.seq === null || wire.grid_epoch === null
    || canonical.seq === null || canonical.grid_epoch === null
    || reconciled.seq === null || reconciled.grid_epoch === null) {
    throw new Error("terminal stream probe omitted an epoch sequence");
  }
  if (options.epoch === "changed") {
    expect(wire.grid_epoch).not.toBe(beforeWire.grid_epoch);
    expect(canonical.grid_epoch).not.toBe(beforeCanonical.grid_epoch);
  } else {
    expect(wire.grid_epoch).toBe(beforeWire.grid_epoch);
    expect(canonical.grid_epoch).toBe(beforeCanonical.grid_epoch);
  }
  expect(wire.seq).toBeGreaterThan(beforeWire.seq);
  expect(canonical.seq).toBeGreaterThan(beforeCanonical.seq);
  expect(canonical.seq).toBeLessThanOrEqual(wire.seq);
  expect(reconciled).toEqual(beforeReconciled);
  expect(pending.browser.presentation?.canonical).toEqual(canonical);
  expect(pending.browser.presentation?.reconciled).toEqual(reconciled);
  expect(pending.browser.presentation?.reader_intent).toBe("reading");
  if (options.readerReason) {
    expect(pending.browser.presentation?.reader_reason).toBe(options.readerReason);
  }
  expect(pending.browser.presentation?.hold_mask).toEqual({
    selection: options.selectionHold ?? true,
    link: false,
  });
  expect(["reader_pending_frame", "selection_hold", "link_hold"])
    .toContain(pending.browser.reconcile_block_reason);
  const beforeRawHead = workerRawHeadSequence(before);
  const rawHead = workerRawHeadSequence(pending);
  const beforeWorkerCell = workerCellSequence(before);
  const workerCell = workerCellSequence(pending);
  const beforeCoordScreen = coordScreenSequence(before);
  const coordScreen = coordScreenSequence(pending);
  if (beforeRawHead === null || rawHead === null
    || beforeWorkerCell === null || workerCell === null
    || beforeCoordScreen === null || coordScreen === null) {
    throw new Error("terminal stream probe omitted a current worker/coordinator sequence");
  }
  expect(rawHead > beforeRawHead).toBe(true);
  expect(workerCell > beforeWorkerCell).toBe(true);
  expect(coordScreen > beforeCoordScreen).toBe(true);
}

export function expectRecoveredLive(
  pending: TerminalStreamProbe,
  recovered: TerminalStreamProbe,
  options: { predictiveCursor?: boolean } = {},
): void {
  const pendingCanonical = pending.browser.handler_canonical;
  const canonical = recovered.browser.handler_canonical;
  if (pendingCanonical.seq === null || canonical.seq === null) {
    throw new Error("terminal stream recovery omitted a canonical sequence");
  }
  expect(canonical.seq).toBeGreaterThanOrEqual(pendingCanonical.seq);
  expect(recovered.browser.dom_reconciled).toEqual(canonical);
  expect(recovered.browser.presentation?.canonical).toEqual(canonical);
  expect(recovered.browser.presentation?.reconciled).toEqual(canonical);
  expect(recovered.browser.presentation?.reader_intent).toBe("live");
  expect(recovered.browser.presentation?.reader_reason).toBeNull();
  expect(recovered.browser.presentation?.hold_mask).toEqual({
    selection: false,
    link: false,
  });
  expect(recovered.browser.presentation?.at_bottom).toBe(true);
  if (options.predictiveCursor) {
    expect([null, "predicted_cursor"]).toContain(recovered.browser.reconcile_block_reason);
  } else {
    expect(recovered.browser.reconcile_block_reason).toBeNull();
  }
}
export function expectPaintedScrollbackWellFormed(
  painted: PaintedScrollbackProbe,
): void {
  expect(Number.isFinite(painted.headSpacerPx)).toBe(true);
  expect(painted.headSpacerPx).toBeGreaterThanOrEqual(0);
  expect(Number.isFinite(painted.tailGapPx)).toBe(true);
  expect(painted.tailGapPx).toBeGreaterThanOrEqual(0);
  const indices = painted.rows.map((row) => row.index);
  expect(indices.every((index) => Number.isSafeInteger(index) && index >= 0)).toBe(true);
  expect(new Set(indices).size).toBe(indices.length);
  expect(indices).toEqual([...indices].sort((left, right) => left - right));
  expect(painted.rows.every((row) => typeof row.text === "string")).toBe(true);
  if (painted.readerAnchor !== null) {
    expect(Number.isSafeInteger(painted.readerAnchor.row)).toBe(true);
    expect(painted.readerAnchor.row).toBeGreaterThanOrEqual(0);
    expect(Number.isFinite(painted.readerAnchor.offsetPx)).toBe(true);
  }
}

export function expectPaintedRowsPreserved(
  before: PaintedScrollbackProbe,
  after: PaintedScrollbackProbe,
  allowLeadingEviction = false,
): void {
  expectPaintedScrollbackWellFormed(before);
  expectPaintedScrollbackWellFormed(after);
  const afterByIndex = new Map(after.rows.map((row) => [row.index, row.text]));
  const retainedFloor = allowLeadingEviction ? (after.rows[0]?.index ?? Number.MAX_SAFE_INTEGER) : 0;
  for (const row of before.rows) {
    if (row.index < retainedFloor) continue;
    expect(
      afterByIndex.get(row.index),
      `painted scrollback row ${row.index} changed or disappeared; after=${JSON.stringify({
        count: after.rows.length,
        first: after.rows[0]?.index,
        last: after.rows.at(-1)?.index,
        headSpacerPx: after.headSpacerPx,
        tailGapPx: after.tailGapPx,
      })}`,
    ).toBe(row.text);
  }
}


function unknownRecord(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === "object" && !Array.isArray(value)
    ? value as Record<string, unknown>
    : null;
}

function diagnosticSequence(value: unknown): bigint | null {
  if (typeof value === "number" && Number.isSafeInteger(value) && value >= 0) return BigInt(value);
  if (typeof value === "string" && /^\d+$/.test(value)) return BigInt(value);
  return null;
}

function workerRawHeadSequence(probe: TerminalStreamProbe): bigint | null {
  const raw = unknownRecord(unknownRecord(probe.worker.session)?.raw);
  return diagnosticSequence(raw?.head_seq);
}

function workerCellSequence(probe: TerminalStreamProbe): bigint | null {
  const cell = unknownRecord(unknownRecord(probe.worker.session)?.cell);
  return diagnosticSequence(cell?.seq);
}

function coordScreenSequence(probe: TerminalStreamProbe): bigint | null {
  const screen = unknownRecord(unknownRecord(probe.coord?.session)?.terminal_screen);
  if (screen?.valid !== true) return null;
  return diagnosticSequence(screen.seq);
}

export interface AcceptedViewGeometry {
  cols: number;
  rows: number;
}

/** Probe view geometry is number|null; specs compare it as settled numbers, so
 *  narrow once here instead of scattering null guards through poll bodies. */
export function acceptedGeometry(
  view: TerminalStreamProbe["browser"]["view"],
): AcceptedViewGeometry | null {
  if (view.status !== "accepted" || view.effective_cols === null || view.effective_rows === null) {
    return null;
  }
  return { cols: view.effective_cols, rows: view.effective_rows };
}


export interface CoordinatorTerminalViewState {
  activeViews: number;
  parkedViews: number;
  streamId: string;
  effective: { cols: number; rows: number } | null;
  unavailable: boolean;
}

function nonNegativeInteger(value: unknown, label: string): number {
  if (typeof value !== "number" || !Number.isSafeInteger(value) || value < 0) {
    throw new Error(`${label} was not a non-negative integer`);
  }
  return value;
}

export function coordinatorTerminalViewState(
  probe: TerminalStreamProbe,
): CoordinatorTerminalViewState | null {
  const session = unknownRecord(probe.coord?.session);
  const terminalView = unknownRecord(session?.terminal_view);
  if (!terminalView) return null;
  const effectiveValue = terminalView.effective;
  const effectiveRecord = effectiveValue === null || effectiveValue === undefined
    ? null
    : unknownRecord(effectiveValue);
  if (effectiveValue !== null && effectiveValue !== undefined && !effectiveRecord) {
    throw new Error("coordinator terminal effective geometry was not an object");
  }
  const streamId = terminalView.streamId;
  if (typeof streamId !== "string") {
    throw new Error("coordinator terminal stream ID was not a string");
  }
  if (typeof terminalView.unavailable !== "boolean") {
    throw new Error("coordinator terminal unavailable state was not a boolean");
  }
  return {
    activeViews: nonNegativeInteger(terminalView.activeViews, "coordinator active view count"),
    parkedViews: nonNegativeInteger(terminalView.parkedViews, "coordinator parked view count"),
    streamId,
    effective: effectiveRecord
      ? {
        cols: nonNegativeInteger(effectiveRecord.cols, "coordinator effective columns"),
        rows: nonNegativeInteger(effectiveRecord.rows, "coordinator effective rows"),
      }
      : null,
    unavailable: terminalView.unavailable,
  };
}
