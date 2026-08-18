import { expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

export async function readTerminalStreamProbe(page: Page, sessionId: string): Promise<TerminalStreamProbe> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalStreamProbe(id);
  }, sessionId);
}

export async function waitForCanonicalAdvance(
  page: Page,
  sessionId: string,
  before: TerminalStreamProbe,
): Promise<TerminalStreamProbe> {
  const floor = Math.max(
    before.browser.wire_received.seq ?? -1,
    before.browser.handler_canonical.seq ?? -1,
  );
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
  if (beforeWire.seq === null || beforeCanonical.seq === null || beforeReconciled.seq === null
    || wire.seq === null || canonical.seq === null || reconciled.seq === null) {
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
  expect(pending.browser.reconcile_block_reason).toBe("reader_pending_frame");
  const beforeRawHead = workerRawHeadSequence(before);
  const rawHead = workerRawHeadSequence(pending);
  const beforeWorkerCell = workerCellSequence(before);
  const workerCell = workerCellSequence(pending);
  const beforeCoordCell = coordCellSequence(before);
  const coordCell = coordCellSequence(pending);
  if (beforeRawHead === null || rawHead === null
    || beforeWorkerCell === null || workerCell === null
    || beforeCoordCell === null || coordCell === null) {
    throw new Error("terminal stream probe omitted a worker/coordinator sequence");
  }
  expect(rawHead > beforeRawHead).toBe(true);
  expect(workerCell > beforeWorkerCell).toBe(true);
  expect(coordCell > beforeCoordCell).toBe(true);
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

function coordCellSequence(probe: TerminalStreamProbe): bigint | null {
  const lastCell = unknownRecord(unknownRecord(probe.coord?.session)?.last_cell);
  return diagnosticSequence(lastCell?.seq);
}

export function activeCoordSubscriptionCount(probe: TerminalStreamProbe): number {
  const session = unknownRecord(probe.coord?.session);
  const subscriptions = unknownRecord(session?.subscriptions);
  if (!subscriptions) return 0;
  return Object.values(subscriptions).filter((value) =>
    unknownRecord(value)?.subscribed === true
  ).length;
}

export function coordViewerCount(probe: TerminalStreamProbe): number {
  const viewers = unknownRecord(unknownRecord(probe.coord?.session)?.viewers);
  return viewers ? Object.keys(viewers).length : 0;
}

export function workerViewerClaimCount(probe: TerminalStreamProbe): number {
  const session = unknownRecord(probe.worker.session);
  const claims = unknownRecord(session?.claims);
  return claims ? Object.keys(claims).length : 0;
}
