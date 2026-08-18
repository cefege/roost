import { mkdirSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { setTimeout as delay } from "node:timers/promises";
import { fromBinary } from "@bufbuild/protobuf";
import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import { dirname, join } from "node:path";
import { FilesListDirRequestSchema } from "../../apps/shared/src/gen/roost/v1/coordinator_pb.ts";
import { encodeFolderPath } from "../../apps/web/src/lib/terminalHref.ts";
import {
  detectBrowserPlatform,
  matchesPlatformShortcut,
  type BrowserPlatform,
  type PlatformShortcutId,
} from "../../apps/web/src/lib/browserPlatform.ts";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import type { PaintedCursorProof, PaintedMarkerProof } from "../../apps/web/src/lib/smokeHarness.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";

const fixturePath = join(dirname(fileURLToPath(import.meta.url)), "resize-tui.ts");

// Playwright's Desktop Chrome profile currently advertises a Windows browser
// even when the test runner itself is hosted elsewhere. Resolve shortcuts from
// the page's navigator and the product's authoritative matcher rather than
// assuming the runner OS or hard-coding one platform's chord.
const shortcutPlatformByPage = new WeakMap<Page, Promise<BrowserPlatform>>();

function shortcutPlatform(page: Page): Promise<BrowserPlatform> {
  let detected = shortcutPlatformByPage.get(page);
  if (!detected) {
    detected = page.evaluate(() => {
      const nav = navigator as Navigator & { userAgentData?: { platform?: string } };
      return {
        userAgent: nav.userAgent,
        platform: nav.platform,
        userAgentData: { platform: nav.userAgentData?.platform },
      };
    }).then(detectBrowserPlatform);
    shortcutPlatformByPage.set(page, detected);
  }
  return detected;
}

async function pressPlatformShortcut(
  page: Page,
  shortcut: PlatformShortcutId,
  key: string,
): Promise<void> {
  const platform = await shortcutPlatform(page);
  // Put the native primary modifier first so the first accepted combination is
  // the same idiomatic chord exposed by the product (Meta outside Windows).
  const modifierOrder = platform === "windows"
    ? ["Alt", "Control", "Shift", "Meta"] as const
    : ["Meta", "Control", "Alt", "Shift"] as const;
  for (let mask = 0; mask < 1 << modifierOrder.length; mask += 1) {
    const modifiers = modifierOrder.filter((_, index) => (mask & (1 << index)) !== 0);
    if (!matchesPlatformShortcut({
      key,
      ctrlKey: modifiers.includes("Control"),
      altKey: modifiers.includes("Alt"),
      shiftKey: modifiers.includes("Shift"),
      metaKey: modifiers.includes("Meta"),
      getModifierState: () => false,
    }, shortcut, platform)) continue;
    await page.keyboard.press([...modifiers, key].join("+"));
    return;
  }
  throw new Error(`No ${platform} keyboard chord matches ${shortcut} with key ${key}`);
}

interface RecoveryMarkerScan {
  total: number; unique: number; min: number; max: number;
  duplicated: number[]; missing: number; outOfOrder: number; firstInversion: number;
}

interface TerminalInputCapture {
  batches: Array<{ sessionId: string; data: number[] }>;
  droppedBatches: number;
}

interface SmokeSessionProjection {
  id: string;
  worker_fp: string;
  cwd?: string;
  spawn_cwd?: string;
}

interface RecoverySmokeApi {
  spawnShell(workerFp: string, folder: string, sessionId?: string): Promise<{ session_id: string; channel_id: number }>;
  state(): {
    sessions: Record<string, SmokeSessionProjection>;
    workers: Record<string, unknown>;
  };
  createWorkspace(workerFp: string, folder: string, sessionId: string): Promise<{ id: string; channel: number }>;
  navigate(href: string): void;
  input(sessionId: string, text: string): Promise<void>;
  paneFocused(sessionId: string): { hasSlot: boolean; hasTextarea: boolean; focused: boolean };
  terminalInputCapture(): TerminalInputCapture;
  resetTerminalInputCapture(): void;
  dropNextCellFrame(sessionId: string): void;
  droppedCellFrameCount(sessionId: string): number;
  cellFrameCount(sessionId: string): number;
  cellFullFrameCount(sessionId: string): number;
  cellGridEpoch(sessionId: string): string;
  lastFullFrameSbRows(sessionId: string): number;
  scrollbackBackfillRequestCount(sessionId: string): number;
  syncWsGeneration(): number;
  pauseSyncTransport(): void;
  resumeSyncTransport(): void;
  forceSyncMaxBackoff(): void;
  syncRedialStatus(): {
    failures: number;
    nextDelayMs: number;
    hiddenParked: boolean;
    liveness: "none" | "dialing" | "open";
  };
  forceHidden(on: boolean): void;
  forceVisible(on: boolean): void;
  viewportText(sessionId: string): string;
  markerScan(sessionId: string, prefix: string): RecoveryMarkerScan;
  renderProbe(sessionId: string): { atBottom: boolean };
  terminalStreamProbe(sessionId: string): Promise<TerminalStreamProbe>;
  waitForPaintedMarker(sessionId: string, marker: string, timeoutMs?: number): Promise<PaintedMarkerProof>;
  waitForPaintedCursor(
    sessionId: string,
    expected?: { row?: number; column?: number },
    timeoutMs?: number,
  ): Promise<PaintedCursorProof>;
  rejectNextViewportClaim(sessionId: string): void;
  rejectedViewportClaimCount(sessionId: string): number;
}

interface TerminalIdentityProbeWindow {
  __smoke: RecoverySmokeApi;
  __terminalIdentityProbe: { slot: Element; grid: Element; textarea: Element };
}

interface RecoveryProbeResult {
  canary: string | null;
  scan: RecoveryMarkerScan;
  atBottom: boolean;
}

type PaintAttempt<T> =
  | { proof: T; error: null }
  | { proof: null; error: string };

interface ImmediateTerminalPaintSample {
  eventType: string;
  trusted: boolean;
  selectionCollapsed: boolean;
  cursorRow: number | null;
  cursorColumn: number | null;
  cursorRect: { left: number; top: number; right: number; bottom: number } | null;
  markerRowRect: { left: number; top: number; right: number; bottom: number } | null;
  composerHeight: number | null;
  cursorRowIdentity: boolean | null;
}

function fixtureWorkerFolder(worker: TerminalTestWorker): string {
  return process.platform === "win32" ? worker.home.replaceAll("\\", "/") : worker.home;
}

async function spawnPtyFixtureSession(page: Page, worker: TerminalTestWorker): Promise<string> {
  await page.waitForFunction((workerFp) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().workers[workerFp];
  }, worker.workerFp);
  return page.evaluate(async ({ workerFp, folder }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, folder)).session_id;
  }, { workerFp: worker.workerFp, folder: fixtureWorkerFolder(worker) });
}

async function readTerminalStreamProbe(page: Page, sessionId: string): Promise<TerminalStreamProbe> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalStreamProbe(id);
  }, sessionId);
}

async function waitForCanonicalAdvance(
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

async function holdNativeTerminalSelection(page: Page, sessionId: string): Promise<boolean> {
  return page.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".wterm");
    if (!(terminal instanceof HTMLElement)) return false;
    const clip = terminal.getBoundingClientRect();
    const rows = Array.from(slot?.querySelectorAll(".cell-row") ?? []);
    const row = rows.find((candidate) => {
      if (!(candidate instanceof HTMLElement) || (candidate.textContent ?? "").length === 0) return false;
      const rect = candidate.getBoundingClientRect();
      return rect.height > 0 && rect.width > 0 && rect.bottom > clip.top && rect.top < clip.bottom;
    });
    if (!row) return false;
    const walker = document.createTreeWalker(row, NodeFilter.SHOW_TEXT);
    let text = walker.nextNode();
    while (text && !text.textContent) text = walker.nextNode();
    if (!text?.textContent) return false;
    const range = document.createRange();
    range.setStart(text, 0);
    range.setEnd(text, 1);
    const selection = window.getSelection();
    selection?.removeAllRanges();
    selection?.addRange(range);
    document.dispatchEvent(new Event("selectionchange"));
    return !!selection && !selection.isCollapsed;
  }, sessionId);
}

async function attemptPaintedMarker(
  page: Page,
  sessionId: string,
  marker: string,
  timeoutMs = 750,
): Promise<PaintAttempt<PaintedMarkerProof>> {
  return page.evaluate(async ({ id, expected, timeout }) => {
    try {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const proof = await smokeWindow.__smoke.waitForPaintedMarker(id, expected, timeout);
      return { proof, error: null };
    } catch (error) {
      return { proof: null, error: error instanceof Error ? error.message : String(error) };
    }
  }, { id: sessionId, expected: marker, timeout: timeoutMs });
}

async function attemptPaintedCursor(
  page: Page,
  sessionId: string,
  expected: { row?: number; column?: number },
  timeoutMs = 750,
): Promise<PaintAttempt<PaintedCursorProof>> {
  return page.evaluate(async ({ id, target, timeout }) => {
    try {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      const proof = await smokeWindow.__smoke.waitForPaintedCursor(id, target, timeout);
      return { proof, error: null };
    } catch (error) {
      return { proof: null, error: error instanceof Error ? error.message : String(error) };
    }
  }, { id: sessionId, target: expected, timeout: timeoutMs });
}

async function armImmediateTerminalPaintSample(
  page: Page,
  sessionId: string,
  eventType: "click" | "input" | "keydown" | "paste" | "wheel",
  expected: { marker?: string; cursorRow?: number; targetTestId?: string },
): Promise<void> {
  await page.evaluate(({ id, observedEvent, marker, cursorRow, targetTestId }) => {
    interface ImmediatePaintRuntime {
      sample: ImmediateTerminalPaintSample | null;
      cleanup: (() => void) | null;
    }
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: ImmediatePaintRuntime;
    };
    runtimeWindow.__immediateTerminalPaint?.cleanup?.();
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".cell-grid");
    const viewport = terminal?.querySelector(".cell-viewport");
    if (
      !(slot instanceof HTMLElement)
      || !(terminal instanceof HTMLElement)
      || !(viewport instanceof HTMLElement)
    ) throw new Error("terminal paint sample could not resolve the live cell surface");

    const originalCursorRow = cursorRow === undefined
      ? null
      : viewport.querySelectorAll(".cell-row").item(cursorRow);
    const rect = (value: DOMRect) => ({
      left: value.left,
      top: value.top,
      right: value.right,
      bottom: value.bottom,
    });
    const visiblyInside = (element: HTMLElement): boolean => {
      const value = element.getBoundingClientRect();
      const clip = terminal.getBoundingClientRect();
      const viewportLeft = window.visualViewport?.offsetLeft ?? 0;
      const viewportTop = window.visualViewport?.offsetTop ?? 0;
      const viewportRight = viewportLeft + (window.visualViewport?.width ?? window.innerWidth);
      const viewportBottom = viewportTop + (window.visualViewport?.height ?? window.innerHeight);
      return value.width > 0
        && value.height > 0
        && value.right > Math.max(clip.left, viewportLeft)
        && value.left < Math.min(clip.right, viewportRight)
        && value.bottom > Math.max(clip.top, viewportTop)
        && value.top < Math.min(clip.bottom, viewportBottom);
    };
    const runtime: ImmediatePaintRuntime = { sample: null, cleanup: null };
    const onEvent = (event: Event) => {
      if (!(event.target instanceof Node)) return;
      const outsideTarget = event.target instanceof Element && targetTestId !== undefined
        ? event.target.closest(`[data-testid="${CSS.escape(targetTestId)}"]`)
        : null;
      if (!slot.contains(event.target) && !outsideTarget) return;
      document.removeEventListener(observedEvent, onEvent);
      runtime.cleanup = null;
      const cursor = viewport.querySelector(".cell-cursor");
      const markerRow = marker === undefined
        ? null
        : Array.from(terminal.querySelectorAll(".cell-row")).find((row) =>
          (row.textContent ?? "").includes(marker));
      runtime.sample = {
        eventType: event.type,
        trusted: event.isTrusted,
        selectionCollapsed: window.getSelection()?.isCollapsed ?? true,
        cursorRow: cursor instanceof HTMLElement && Number.isSafeInteger(Number(cursor.dataset.row))
          ? Number(cursor.dataset.row)
          : null,
        cursorColumn: cursor instanceof HTMLElement && Number.isSafeInteger(Number(cursor.dataset.column))
          ? Number(cursor.dataset.column)
          : null,
        cursorRect: cursor instanceof HTMLElement && visiblyInside(cursor)
          ? rect(cursor.getBoundingClientRect())
          : null,
        markerRowRect: markerRow instanceof HTMLElement && visiblyInside(markerRow)
          ? rect(markerRow.getBoundingClientRect())
          : null,
        composerHeight: (() => {
          const composer = slot.querySelector('[data-testid="mobile-chat-input"]');
          return composer instanceof HTMLElement ? composer.getBoundingClientRect().height : null;
        })(),
        cursorRowIdentity: cursorRow === undefined
          ? null
          : originalCursorRow !== null
            && viewport.querySelectorAll(".cell-row").item(cursorRow) === originalCursorRow,
      };
    };
    runtime.cleanup = () => document.removeEventListener(observedEvent, onEvent);
    runtimeWindow.__immediateTerminalPaint = runtime;
    document.addEventListener(observedEvent, onEvent);
  }, {
    id: sessionId,
    observedEvent: eventType,
    marker: expected.marker,
    cursorRow: expected.cursorRow,
    targetTestId: expected.targetTestId,
  });
}

/** Chromium delivers a dispatched input event to JS asynchronously: the
 *  `mouse.wheel`/`keyboard` call resolves on the CDP ack, not on the listener
 *  having run. Reading the armed sample straight after the gesture therefore
 *  races it and observes null under load. Wait for the sample the arm exposes,
 *  then read it — still null after the budget means the gesture genuinely never
 *  reached the pane. */
async function readImmediateTerminalPaintSample(page: Page): Promise<ImmediateTerminalPaintSample | null> {
  await page.waitForFunction(() => {
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: { sample: ImmediateTerminalPaintSample | null };
    };
    return runtimeWindow.__immediateTerminalPaint?.sample != null;
  }, undefined, { timeout: 5_000 }).catch(() => undefined);
  return page.evaluate(() => {
    const runtimeWindow = window as unknown as Window & {
      __immediateTerminalPaint?: { sample: ImmediateTerminalPaintSample | null };
    };
    return runtimeWindow.__immediateTerminalPaint?.sample ?? null;
  });
}

function expectCanonicalAdvanceHeld(
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

function expectRecoveredLive(
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

function activeCoordSubscriptionCount(probe: TerminalStreamProbe): number {
  const session = unknownRecord(probe.coord?.session);
  const subscriptions = unknownRecord(session?.subscriptions);
  if (!subscriptions) return 0;
  return Object.values(subscriptions).filter((value) =>
    unknownRecord(value)?.subscribed === true
  ).length;
}

function coordViewerCount(probe: TerminalStreamProbe): number {
  const viewers = unknownRecord(unknownRecord(probe.coord?.session)?.viewers);
  return viewers ? Object.keys(viewers).length : 0;
}

function workerViewerClaimCount(probe: TerminalStreamProbe): number {
  const session = unknownRecord(probe.worker.session);
  const claims = unknownRecord(session?.claims);
  return claims ? Object.keys(claims).length : 0;
}


async function spawnSmokeShell(page: Page, workerFp: string, sessionId?: string) {
  return page.evaluate(async ({ workerFp: fp, sessionId: sid }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    const session = await smoke.spawnShell(fp, "/tmp", sid);
    await smoke.createWorkspace(fp, "/tmp", session.session_id);
    return session;
  }, { workerFp, sessionId });
}

async function navigateToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.goto(`${new URL(page.url()).origin}/s/${sessionId}`, { waitUntil: "domcontentloaded" });
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
}

async function switchToSmokeSession(page: Page, sessionId: string): Promise<void> {
  await page.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.navigate(`/s/${id}`),
    sessionId,
  );
  await expect(page.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
}

async function expectSmokeComposer(page: Page): Promise<void> {
  await expect(page.getByTestId("mobile-chat-input")).toBeVisible();
  await expect(page.getByTestId("chat-box")).toBeVisible();
  await expect(page.getByTestId("chat-input")).toBeVisible();
}

async function readWorkerBytes(
  client: TerminalTestStack["client"],
  workerFp: string,
  path: string,
): Promise<Uint8Array> {
  const parts: Uint8Array[] = [];
  let offset = 0;
  for (;;) {
    const response = await client.filesReadChunk({
      workerFp,
      path,
      offset: BigInt(offset),
      len: 4 * 1024 * 1024,
    });
    if (response.data.length > 0) {
      parts.push(response.data);
      offset += response.data.length;
    }
    if (response.eof || response.data.length === 0) break;
  }
  const bytes = new Uint8Array(offset);
  let writeOffset = 0;
  for (const part of parts) {
    bytes.set(part, writeOffset);
    writeOffset += part.length;
  }
  return bytes;
}

async function readStoredComposerDraft(
  page: Page,
  sessionId: string,
): Promise<{ present: boolean; value: string | null }> {
  return page.evaluate((id) => {
    try {
      const raw = localStorage.getItem("roost.composerDrafts.v1");
      const parsed: unknown = raw ? JSON.parse(raw) : {};
      if (!parsed || typeof parsed !== "object" || !Object.prototype.hasOwnProperty.call(parsed, id)) {
        return { present: false, value: null };
      }
      const value = Reflect.get(parsed, id);
      return { present: true, value: typeof value === "string" ? value : null };
    } catch {
      return { present: false, value: null };
    }
  }, sessionId);
}
async function inputSmokeTerminal(page: Page, sessionId: string, text: string): Promise<void> {
  await page.evaluate(async ({ id, input }) => {
    // The smoke backdoor is injected only in smoke-enabled browser contexts.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, input);
  }, { id: sessionId, input: text });
}

async function resetTerminalInputCapture(page: Page): Promise<void> {
  await page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.resetTerminalInputCapture();
  });
}

async function readTerminalInputCapture(page: Page): Promise<TerminalInputCapture> {
  return page.evaluate(() => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalInputCapture();
  });
}


async function waitForStableCellFrames(page: Page, sessionId: string): Promise<void> {
  let previous = -1;
  let unchangedPolls = 0;
  await expect.poll(async () => {
    const current = await page.evaluate(
      (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
      sessionId,
    );
    if (current === previous) unchangedPolls += 1;
    else {
      previous = current;
      unchangedPolls = 0;
    }
    return unchangedPolls;
  }, { timeout: 3_000, intervals: [50] }).toBeGreaterThanOrEqual(3);
}

async function setRecoveryCanary(page: Page, canary: string): Promise<void> {
  await page.evaluate((value) => { document.documentElement.dataset.terminalStreamCanary = value; }, canary);
}

async function recoveryProbe(page: Page, sessionId: string, prefix: string): Promise<RecoveryProbeResult> {
  return page.evaluate(({ sessionId: id, prefix: markerPrefix }) => ({
    canary: document.documentElement.dataset.terminalStreamCanary ?? null,
    scan: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, markerPrefix),
    atBottom: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.renderProbe(id).atBottom,
  }), { sessionId, prefix });
}

function expectCleanRecovery(
  result: RecoveryProbeResult,
  canary: string,
  min: number,
  max: number,
): void {
  expect(result.canary).toBe(canary);
  expect(result.scan).toMatchObject({
    min,
    max,
    duplicated: [],
    missing: 0,
    outOfOrder: 0,
  });
  expect(result.atBottom).toBe(true);
}
test("browser smoke flow creates and cleans its resources", async ({ smokePage, stack }) => {
  // Pin the shell worker: the shared stack also runs a PTY-FIXTURE worker whose
  // "shell" speaks the fixture protocol, and picking it by recency makes the
  // flow wait out its paint deadline on a session that can never echo.
  const result = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runFlow(options?: { workerFp?: string }): Promise<{
          steps: Array<{ name: string; pass: boolean; detail: unknown }>;
          summary: string;
        }>;
      };
    }).__smoke;
    return smoke.runFlow({ workerFp });
  }, stack.workerFp);
  expect(result.steps.filter((step) => !step.pass)).toEqual([]);
});

test("new-terminal server switch resets browse path before listing and spawning", async ({
  multiWorkerSmokePage,
  stack,
  secondWorker,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop multi-worker browse contract");

  const suffix = crypto.randomUUID().slice(0, 8);
  const aChildName = `a-only-${suffix}`;
  const aChildPath = join(stack.workerHome, aChildName);
  const bDefaultPath = join(secondWorker.home, `b-default-${suffix}`);
  mkdirSync(aChildPath, { recursive: true });
  mkdirSync(bDefaultPath, { recursive: true });

  const bSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, bSeedSessionId);

  // FlatNewTerminal chooses the globally newest session. Cross a timestamp
  // boundary, then seed A so the sidebar-scoped plus deterministically opens A.
  await delay(10);
  const aSeedSessionId = await multiWorkerSmokePage.evaluate(async ({ workerFp, cwd }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return (await smokeWindow.__smoke.spawnShell(workerFp, cwd)).session_id;
  }, { workerFp: stack.workerFp, cwd: stack.workerHome });
  await multiWorkerSmokePage.waitForFunction((sessionId) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return !!smokeWindow.__smoke.state().sessions[sessionId]?.cwd;
  }, aSeedSessionId);

  const seedCwds = await multiWorkerSmokePage.evaluate(({ aId, bId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return { a: sessions[aId]?.cwd, b: sessions[bId]?.cwd };
  }, { aId: aSeedSessionId, bId: bSeedSessionId });
  expect(seedCwds).toEqual({ a: stack.workerHome, b: bDefaultPath });

  const listRequests: Array<{ workerFp: string; path: string }> = [];
  const decodeErrors: string[] = [];
  multiWorkerSmokePage.on("request", (request) => {
    if (!new URL(request.url()).pathname.endsWith("/roost.v1.CoordinatorService/FilesListDir")) return;
    const body = request.postDataBuffer();
    if (!body) {
      decodeErrors.push("FilesListDir request had no body");
      return;
    }
    try {
      const decoded = fromBinary(FilesListDirRequestSchema, body);
      listRequests.push({ workerFp: decoded.workerFp, path: decoded.path });
    } catch (error) {
      decodeErrors.push(String(error));
    }
  });

  await multiWorkerSmokePage
    .getByTestId("folder-list")
    .getByTestId("flat-new-terminal-button")
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${stack.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", "roost-terminal-test");
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", stack.workerHome);

  const aFolder = multiWorkerSmokePage
    .locator('[data-testid="browse-tile"], [data-testid="browse-row"]')
    .filter({ hasText: aChildName });
  await expect(aFolder).toHaveCount(1);
  await aFolder.click();
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", aChildPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeEnabled();

  await multiWorkerSmokePage.getByTestId("browse-server").click();
  await multiWorkerSmokePage
    .getByTestId("browse-server-option")
    .filter({ hasText: secondWorker.label })
    .click();
  await expect(multiWorkerSmokePage).toHaveURL(`${stack.baseUrl}/browse/${secondWorker.workerFp}`);
  await expect(multiWorkerSmokePage.getByTestId("browse-server")).toHaveAttribute("title", secondWorker.label);
  await expect(multiWorkerSmokePage.getByTestId("browse-crumb").last()).toHaveAttribute("title", bDefaultPath);
  await expect(multiWorkerSmokePage.getByTestId("browse-back")).toBeDisabled();

  await expect.poll(
    () => listRequests.some((request) =>
      request.workerFp === secondWorker.workerFp && request.path === bDefaultPath),
  ).toBe(true);
  expect(decodeErrors).toEqual([]);
  expect(listRequests).not.toContainEqual({ workerFp: secondWorker.workerFp, path: aChildPath });

  await multiWorkerSmokePage.getByTestId("browse-open").click();
  await expect(multiWorkerSmokePage).toHaveURL(
    `${stack.baseUrl}/t/${secondWorker.workerFp}/${encodeFolderPath(bDefaultPath)}`,
  );
  await expect.poll(() => multiWorkerSmokePage.evaluate(({ workerFp, cwd, seedId }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const sessions = smokeWindow.__smoke.state().sessions;
    return Object.values(sessions).filter((session) =>
      session.id !== seedId
      && session.worker_fp === workerFp
      && session.cwd === cwd
      && session.spawn_cwd === cwd
    ).length;
  }, { workerFp: secondWorker.workerFp, cwd: bDefaultPath, seedId: bSeedSessionId })).toBe(1);
});

test("dropped initial full frame reclaims immediately on the first delta", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  const sessionId = crypto.randomUUID();
  const canary = `initial-full-${sessionId}`;
  const spawned = await smokePage.evaluate(async ({ workerFp, sessionId: id }) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    return smoke.spawnShell(workerFp, "/tmp", id);
  }, { workerFp: stack.workerFp, sessionId });
  expect(spawned.session_id).toBe(sessionId);
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "printf 'INITIAL-RECOVER-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toContain("INITIAL-RECOVER-001");
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "INITIAL-RECOVER-"), canary, 1, 1);
});

test("dropped streaming delta recovers before the producer goes quiet", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  const canary = `middle-delta-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "i=1; while [ \"$i\" -le 80 ]; do printf 'STREAM-RECOVER-%03d\\n' \"$i\"; i=$((i+1)); sleep 0.03; done\r",
    );
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 10_000, intervals: [50] }).toBeGreaterThanOrEqual(20);
  const earlyMax = await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  );
  expect(earlyMax).toBeLessThan(80);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.markerScan(id, "STREAM-RECOVER-").max,
    sessionId,
  ), { timeout: 15_000, intervals: [50] }).toBe(80);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "STREAM-RECOVER-"), canary, 1, 80);
});

test("dropped final frame is repaired by the applied-sequence heartbeat", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell recovery contract");
  test.setTimeout(75_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "stty -echo; printf 'HEARTBEAT-READY-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("HEARTBEAT-READY-001");
  await expect.poll(() => smokePage.evaluate((id) => {
    // The test bootstrap installs this typed in-process harness on window.
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return /HEARTBEAT-READY-001bash-\d+(?:\.\d+)+\$/.test(
      smokeWindow.__smoke.viewportText(id),
    );
  }, sessionId)).toBe(true);
  await waitForStableCellFrames(smokePage, sessionId);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "read _; printf 'HEARTBEAT-RECOVER-%03d\\n' 1; read _\r",
    ),
    sessionId,
  );
  await waitForStableCellFrames(smokePage, sessionId);
  const canary = `heartbeat-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => ({
    frames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(id, "go\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.droppedCellFrameCount(id),
    sessionId,
  )).toBe(1);
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).not.toContain("HEARTBEAT-RECOVER-001");

  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 45_000, intervals: [250] }).toContain("HEARTBEAT-RECOVER-001");
  const after = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(after.fullFrames).toBe(before.fullFrames + 1);
  expect(after.wsGeneration).toBe(before.wsGeneration);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HEARTBEAT-RECOVER-"), canary, 1, 1);
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    await smoke.input(id, "go\r");
    await smoke.input(id, "stty echo\r");
  }, sessionId);
});

test("parking a selection-held pane flushes its latest folded frame", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop paint-hold contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  const canary = `selection-hold-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const selected = await holdNativeTerminalSelection(smokePage, sessionId);
  expect(selected).toBe(true);
  const beforeFrames = await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    sessionId,
  );
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(id, "printf 'HOLD-RECOVER-%03d\\n' 1\r"),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFrameCount(id),
    sessionId,
  )).toBeGreaterThan(beforeFrames);
  expect(await smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .not.toContain("HOLD-RECOVER-001");

  const otherSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, otherSessionId);
  await switchToSmokeSession(smokePage, sessionId);
  // Report the layer that stalled instead of just "text missing": wire vs
  // canonical vs DOM plus the reason the renderer refused to reconcile.
  await expect.poll(async () => {
    const text = await smokePage.evaluate(
      (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
      sessionId,
    );
    if (text.includes("HOLD-RECOVER-001")) return "painted";
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      text,
      wire: probe.browser.wire_received,
      canonical: probe.browser.handler_canonical,
      dom: probe.browser.dom_reconciled,
      blocked: probe.browser.reconcile_block_reason,
    };
  }, { timeout: 10_000 }).toBe("painted");
  expect(await smokePage.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(true);
  expectCleanRecovery(await recoveryProbe(smokePage, sessionId, "HOLD-RECOVER-"), canary, 1, 1);
});

test("same session metadata updates preserve the mounted terminal DOM", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop terminal identity contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    return smokeWindow.__smoke.cellFullFrameCount(id);
  }, sessionId)).toBeGreaterThan(0);

  await smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    const grid = slot?.querySelector(".cell-grid");
    const textarea = slot?.querySelector("textarea");
    if (!slot || !grid || !textarea) throw new Error("terminal DOM identity probe could not be installed");
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    smokeWindow.__terminalIdentityProbe = { slot, grid, textarea };
  }, sessionId);

  // Folder rows collapse sessions by cwd; search switches the sidebar to the
  // per-session rows that carry the rename menu (same surface agent-status uses).
  await smokePage.getByTestId("brand-row-search").click();
  await smokePage.getByTestId("sidebar-search").fill("/tmp");
  const sessionRow = smokePage.locator(
    `[data-testid="sidebar-session-row"][data-session-id="${sessionId}"]`,
  );
  await sessionRow.click({ button: "right" });
  await smokePage.getByTestId(`session-ctx-rename-${sessionId}`).click();
  const renameInput = smokePage.getByTestId("rename-input");
  const customTitle = `identity-${sessionId.slice(0, 8)}`;
  await renameInput.evaluate((element, title) => {
    const field = element as HTMLElement & { value: string };
    field.value = title;
    field.dispatchEvent(new Event("input", { bubbles: true, composed: true }));
  }, customTitle);
  await smokePage.getByTestId("rename-confirm").click();
  await expect(sessionRow).toContainText(customTitle);

  expect(await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    const prior = smokeWindow.__terminalIdentityProbe;
    const slot = document.querySelector(`[data-testid="terminal-slot-${CSS.escape(id)}"]`);
    return {
      slot: slot === prior.slot,
      grid: slot?.querySelector(".cell-grid") === prior.grid,
      textarea: slot?.querySelector("textarea") === prior.textarea,
      connected: prior.slot.isConnected && prior.grid.isConnected && prior.textarea.isConnected,
    };
  }, sessionId)).toEqual({
    slot: true,
    grid: true,
    textarea: true,
    connected: true,
  });

  const marker = `IDENTITY-SURVIVED-${sessionId}`;
  await smokePage.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as TerminalIdentityProbeWindow;
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: `printf '${marker}\\n'\r` });
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .toContain(marker);
});

test("real PTY input recovers held rendering and rejected same-generation reclaim self-heals", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop terminal recovery and reclaim reproduction");
  test.setTimeout(180_000);

  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const grid = slot.locator(".cell-grid");
  await grid.click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 12);
  const historyPrefix = `RECOVERY-HISTORY-${suffix}-`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix: historyPrefix, count: 96 }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: `${historyPrefix}96` });

  // A smoke-backdoor byte is deliberately passive. The fixture answers it with
  // CSI 1 C (cursor-forward/right), advancing the canonical cursor while the
  // selected reader remains on the old geometry. The next admitted trusted key
  // must adopt that exact pending frame inside keydown, preserve every clean row
  // node, clear only this pane's Selection, and pin.
  const cursorNonce = `cursor-${suffix}`;
  const cursorReady = `ARMED:CURSOR_MOVE:${cursorNonce}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_CURSOR_MOVE", nonce: cursorNonce }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: cursorReady });
  await waitForStableCellFrames(smokePage, sessionId);
  const baselineCursor = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedCursor(id, undefined, 10_000);
  }, sessionId);
  const beforeCursor = await readTerminalStreamProbe(smokePage, sessionId);
  expect(beforeCursor.browser.handler_canonical).toEqual(beforeCursor.browser.dom_reconciled);
  expect(await holdNativeTerminalSelection(smokePage, sessionId)).toBe(true);
  await inputSmokeTerminal(smokePage, sessionId, "x");
  const cursorPending = await waitForCanonicalAdvance(smokePage, sessionId, beforeCursor);
  expectCanonicalAdvanceHeld(beforeCursor, cursorPending, {
    readerReason: "selection",
    selectionHold: true,
  });
  const canonicalCursor = cursorPending.browser.presentation?.cursor.canonical;
  if (!canonicalCursor?.visible) throw new Error("cursor-only fixture did not leave a canonical cursor");
  expect(canonicalCursor.row).toBe(baselineCursor.row);
  expect(canonicalCursor.column).toBe(baselineCursor.column + 1);
  expect(cursorPending.browser.presentation?.cursor.dom).toMatchObject({
    visible: true,
    row: baselineCursor.row,
    column: baselineCursor.column,
    connected: true,
  });
  const staleCursorGeometry = await attemptPaintedCursor(smokePage, sessionId, {
    row: baselineCursor.row,
    column: baselineCursor.column,
  });
  if (!staleCursorGeometry.proof) {
    throw new Error(`baseline cursor lost clipped geometry: ${staleCursorGeometry.error}`);
  }
  expect(staleCursorGeometry.proof.rect).toEqual(baselineCursor.rect);
  expect(staleCursorGeometry.proof.terminalClip).toEqual(baselineCursor.terminalClip);
  expect(staleCursorGeometry.proof.visualViewport).toEqual(baselineCursor.visualViewport);
  const hiddenCanonicalCursor = await attemptPaintedCursor(smokePage, sessionId, {
    row: canonicalCursor.row,
    column: canonicalCursor.column,
  });
  expect(hiddenCanonicalCursor.proof).toBeNull();
  expect(hiddenCanonicalCursor.error).toContain("cursor presentation geometry was not proven");

  // Escape is admitted PTY data without adding a cursor prediction or browser
  // scrolling default; CSI 1 C remains the sole cursor transition.
  await resetTerminalInputCapture(smokePage);
  await armImmediateTerminalPaintSample(smokePage, sessionId, "keydown", {
    cursorRow: canonicalCursor.row,
  });
  await smokePage.keyboard.press("Escape");
  const immediateKeyPaint = await readImmediateTerminalPaintSample(smokePage);
  if (!immediateKeyPaint?.cursorRect) throw new Error("trusted key did not reconcile cursor geometry synchronously");
  expect(immediateKeyPaint).toMatchObject({
    eventType: "keydown",
    trusted: true,
    selectionCollapsed: true,
    cursorRow: canonicalCursor.row,
    cursorColumn: canonicalCursor.column,
    cursorRowIdentity: true,
  });
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(smokePage);
    return {
      data: capture.batches.flatMap((batch) => batch.data),
      sessions: capture.batches.map((batch) => batch.sessionId),
      droppedBatches: capture.droppedBatches,
    };
  }).toEqual({
    data: [0x1b],
    sessions: [sessionId],
    droppedBatches: 0,
  });
  const afterTrustedKey = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterTrustedKey.browser.presentation?.cursor.canonical).toEqual(canonicalCursor);
  expect(afterTrustedKey.browser.presentation?.cursor.dom).toMatchObject({
    ...canonicalCursor,
    connected: true,
  });
  expectRecoveredLive(cursorPending, afterTrustedKey);
  const recoveredCursor = await smokePage.evaluate(({ id, row, column }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedCursor(id, { row, column }, 10_000);
  }, { id: sessionId, row: canonicalCursor.row, column: canonicalCursor.column });
  expect(recoveredCursor).toMatchObject({
    row: canonicalCursor.row,
    column: canonicalCursor.column,
    frames: 2,
  });
  expect(recoveredCursor.terminalClip).toEqual(baselineCursor.terminalClip);
  expect(recoveredCursor.visualViewport).toEqual(baselineCursor.visualViewport);
  for (const key of ["left", "top", "right", "bottom"] as const) {
    expect(
      Math.abs(immediateKeyPaint.cursorRect[key] - recoveredCursor.rect[key]),
      `synchronous cursor ${key}`,
    ).toBeLessThanOrEqual(1);
  }

  const liveAfterKey = `LIVE-AFTER-KEY:${suffix}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: liveAfterKey }),
  );
  const liveAfterKeyProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: liveAfterKey });
  expect(liveAfterKeyProof).toMatchObject({ marker: liveAfterKey, frames: 2 });
  expect((await readTerminalStreamProbe(smokePage, sessionId)).browser.presentation?.reader_intent)
    .toBe("live");

  // Native wheel is explicit reading. Its next frame remains canonical-only;
  // committed IME input resumes live in the input event that admits its bytes.
  const gridBox = await grid.boundingBox();
  if (!gridBox) throw new Error("terminal grid geometry disappeared");
  await smokePage.mouse.move(gridBox.x + gridBox.width / 2, gridBox.y + gridBox.height / 2);
  await smokePage.mouse.wheel(0, -1200);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      intent: probe.browser.presentation?.reader_intent,
      reason: probe.browser.presentation?.reader_reason,
      atBottom: probe.browser.presentation?.at_bottom,
    };
  }).toEqual({ intent: "reading", reason: "wheel", atBottom: false });
  const beforeWheelPending = await readTerminalStreamProbe(smokePage, sessionId);
  const wheelPendingMarker = `WHEEL-PENDING:${suffix}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: wheelPendingMarker }),
  );
  const wheelPending = await waitForCanonicalAdvance(smokePage, sessionId, beforeWheelPending);
  expectCanonicalAdvanceHeld(beforeWheelPending, wheelPending, {
    readerReason: "wheel",
    selectionHold: false,
  });
  const hiddenWheelMarker = await attemptPaintedMarker(smokePage, sessionId, wheelPendingMarker);
  expect(hiddenWheelMarker.proof).toBeNull();
  expect(hiddenWheelMarker.error).toContain("not visibly painted");
  await armImmediateTerminalPaintSample(smokePage, sessionId, "input", {
    marker: wheelPendingMarker,
  });
  await smokePage.keyboard.insertText("中");
  const immediateImePaint = await readImmediateTerminalPaintSample(smokePage);
  expect(immediateImePaint).toMatchObject({
    eventType: "input",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateImePaint?.markerRowRect).not.toBeNull();
  const imeRecoveredProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: wheelPendingMarker });
  expect(imeRecoveredProof).toMatchObject({ marker: wheelPendingMarker, frames: 2 });
  expectRecoveredLive(
    wheelPending,
    await readTerminalStreamProbe(smokePage, sessionId),
    { predictiveCursor: true },
  );

  // Find is another explicit reader action. Closing the bar is passive; a real
  // one-line clipboard paste is the admitted interaction that adopts its pending
  // frame and returns to live.
  await pressPlatformShortcut(smokePage, "terminalFind", "F");
  const findInput = smokePage.getByTestId("terminal-find-input");
  await expect(findInput).toBeVisible();
  const findMarker = `${historyPrefix}20`;
  await findInput.fill(findMarker);
  await expect(smokePage.getByTestId("terminal-find-count")).toHaveText("1/1", { timeout: 10_000 });
  const findProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: findMarker });
  expect(findProof).toMatchObject({ marker: findMarker, frames: 2 });
  await expect.poll(async () => {
    const presentation = (await readTerminalStreamProbe(smokePage, sessionId)).browser.presentation;
    return { intent: presentation?.reader_intent, reason: presentation?.reader_reason };
  }).toEqual({ intent: "reading", reason: "find" });
  const findOpenRows = (await readTerminalStreamProbe(smokePage, sessionId))
    .browser.presentation?.rows.dom;
  if (findOpenRows === undefined) throw new Error("find presentation omitted DOM rows");
  await findInput.press("Escape");
  await expect(findInput).toHaveCount(0);
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(smokePage, sessionId)).browser;
    const presentation = browser.presentation;
    const domRows = presentation?.rows.dom ?? null;
    const canonicalRows = presentation?.rows.canonical ?? null;
    const desiredRows = browser.claim.desired?.rows ?? null;
    const confirmedRows = browser.claim.confirmed?.effective_rows ?? null;
    return {
      resizedBehindReader: canonicalRows !== null
        && canonicalRows > findOpenRows
        && desiredRows === canonicalRows
        && confirmedRows === canonicalRows,
      domRows,
      intent: presentation?.reader_intent,
      reason: presentation?.reader_reason,
      blocked: browser.reconcile_block_reason,
    };
  }).toEqual({
    resizedBehindReader: true,
    domRows: findOpenRows,
    intent: "reading",
    reason: "find",
    blocked: "reader_pending_frame",
  });

  const beforeFindPending = await readTerminalStreamProbe(smokePage, sessionId);
  const findPendingMarker = `FIND-PENDING:${suffix}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: findPendingMarker }),
  );
  const findPending = await waitForCanonicalAdvance(smokePage, sessionId, beforeFindPending);
  expectCanonicalAdvanceHeld(beforeFindPending, findPending, {
    readerReason: "find",
    selectionHold: false,
  });
  const hiddenFindMarker = await attemptPaintedMarker(smokePage, sessionId, findPendingMarker);
  expect(hiddenFindMarker.proof).toBeNull();
  const origin = new URL(smokePage.url()).origin;
  await smokePage.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin });
  const pasteText = `pasted-${suffix}`;
  await smokePage.evaluate((text) => navigator.clipboard.writeText(text), pasteText);
  await resetTerminalInputCapture(smokePage);
  await armImmediateTerminalPaintSample(smokePage, sessionId, "paste", {
    marker: findPendingMarker,
  });
  await smokePage.keyboard.press((await shortcutPlatform(smokePage)) === "macos" ? "Meta+V" : "Control+V");
  const immediatePastePaint = await readImmediateTerminalPaintSample(smokePage);
  expect(immediatePastePaint).toMatchObject({
    eventType: "paste",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediatePastePaint?.markerRowRect).not.toBeNull();
  const pasteRecoveredProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: findPendingMarker });
  expect(pasteRecoveredProof).toMatchObject({ marker: findPendingMarker, frames: 2 });
  await expect.poll(async () =>
    (await readTerminalInputCapture(smokePage)).batches.flatMap((batch) => batch.data)
  ).toEqual(Array.from(new TextEncoder().encode(pasteText)));
  expectRecoveredLive(
    findPending,
    await readTerminalStreamProbe(smokePage, sessionId),
    { predictiveCursor: true },
  );

  // Grow the desktop composer and let its debounced viewport transaction settle
  // before creating the changed-epoch frame. The core's resize snapshot is an
  // independent canonical frame; the interaction contract below starts from the
  // newest stable geometry and proves Send adopts the later held alt-screen.
  const composerDock = slot.getByTestId("mobile-chat-input");
  const composerInput = composerDock.getByTestId("chat-input");
  const composerSend = composerDock.getByTestId("chat-send");
  const restingComposer = await composerDock.boundingBox();
  if (!restingComposer) throw new Error("desktop composer geometry disappeared");
  const pendingSubmitDraft = [
    "accepted submit adopts the changed epoch",
    "before its composer can shrink",
    "without waiting for another output frame",
    "and leaves the pane persistently live",
  ].join("\n");
  await composerInput.fill(pendingSubmitDraft);
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? 0)
    .toBeGreaterThan(restingComposer.height + 1);
  const grownBeforePendingSubmit = await composerDock.boundingBox();
  if (!grownBeforePendingSubmit) throw new Error("grown desktop composer geometry disappeared");
  await waitForStableCellFrames(smokePage, sessionId);

  // A changed-epoch alternate-screen response is now pending before Send. The
  // admitted callback must paint it inside the click dispatch while the tall
  // composer is still at its pre-shrink geometry.
  const altNonce = `alt-${suffix}`;
  const altReady = `ARMED:ALT_REDRAW:${altNonce}:line`;
  const altMarker = `ALT_REDRAW:${altNonce}:1:alt`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_ALT_REDRAW", nonce: altNonce, trigger: "line" }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: altReady });
  await waitForStableCellFrames(smokePage, sessionId);
  const beforeAlt = await readTerminalStreamProbe(smokePage, sessionId);
  expect(await holdNativeTerminalSelection(smokePage, sessionId)).toBe(true);
  await inputSmokeTerminal(smokePage, sessionId, "passive changed epoch\r");
  const altPending = await waitForCanonicalAdvance(smokePage, sessionId, beforeAlt);
  expectCanonicalAdvanceHeld(beforeAlt, altPending, {
    epoch: "changed",
    readerReason: "selection",
    selectionHold: true,
  });
  expect(altPending.browser.presentation?.mode.canonical?.alt_screen).toBe(true);
  expect(altPending.browser.presentation?.mode.reconciled?.alt_screen).toBe(false);
  const hiddenAltMarker = await attemptPaintedMarker(smokePage, sessionId, altMarker);
  expect(hiddenAltMarker.proof).toBeNull();
  expect(await smokePage.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(false);
  expect((await readTerminalStreamProbe(smokePage, sessionId)).browser.presentation)
    .toMatchObject({
      reader_intent: "reading",
      reader_reason: "selection",
      hold_mask: { selection: true, link: false },
    });
  await armImmediateTerminalPaintSample(smokePage, sessionId, "click", { marker: altMarker });
  await composerSend.click();
  const immediateSubmitPaint = await readImmediateTerminalPaintSample(smokePage);
  expect(immediateSubmitPaint).toMatchObject({
    eventType: "click",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateSubmitPaint?.markerRowRect).not.toBeNull();
  expect(immediateSubmitPaint?.composerHeight).toBeGreaterThanOrEqual(grownBeforePendingSubmit.height - 1);
  const altRecoveredProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: altMarker });
  expect(altRecoveredProof).toMatchObject({ marker: altMarker, frames: 2 });
  await expect(composerInput).toHaveValue("");
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? Number.POSITIVE_INFINITY)
    .toBeLessThanOrEqual(restingComposer.height + 1);
  expectRecoveredLive(altPending, await readTerminalStreamProbe(smokePage, sessionId));

  // Reverse the ordering: hold fixture output behind a deterministic delay so
  // accepted admission clears and shrinks the composer before the response.
  // The later marker must still paint immediately because live intent persists.
  const overwriteNonce = `overwrite-${suffix}`;
  const overwriteReady = `ARMED:LINE_OVERWRITE:${overwriteNonce}`;
  const overwriteMarker = `OVERWRITE:${overwriteNonce}:1`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_LINE_OVERWRITE", nonce: overwriteNonce }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: overwriteReady });
  const delayedDraft = `delayed-response-${suffix}-` + "payload ".repeat(80);
  await composerInput.fill(delayedDraft);
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? 0)
    .toBeGreaterThan(restingComposer.height + 1);
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({
      op: "EMIT",
      text: `DELAY-BARRIER:${suffix}`,
      delayMs: 2_500,
    }),
  );
  await composerSend.click();
  await expect(composerInput).toHaveValue("");
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? Number.POSITIVE_INFINITY)
    .toBeLessThanOrEqual(restingComposer.height + 1);
  const beforeDelayedResponse = await attemptPaintedMarker(smokePage, sessionId, overwriteMarker, 250);
  expect(beforeDelayedResponse.proof).toBeNull();
  const overwritePainted = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: overwriteMarker });
  expect(overwritePainted).toMatchObject({ marker: overwriteMarker, frames: 2 });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const desired = probe.browser.claim.desired;
    const confirmed = probe.browser.claim.confirmed;
    return desired !== null
      && confirmed?.client_seq === desired.client_seq
      && activeCoordSubscriptionCount(probe) > 0
      && coordViewerCount(probe) > 0
      && workerViewerClaimCount(probe) > 0;
  }, { timeout: 10_000, intervals: [100] }).toBe(true);
  const presented = await readTerminalStreamProbe(smokePage, sessionId);
  expect(presented.browser.handler_canonical).toEqual(presented.browser.dom_reconciled);
  expect(presented.browser.presentation).toMatchObject({
    reader_intent: "live",
    reader_reason: null,
    hold_mask: { selection: false, link: false },
  });
  expect(presented.browser.last_geometry_proof).toMatchObject({
    proof_kind: "marker",
    marker: overwriteMarker,
    frames: 2,
  });
  expect(presented.session_id).toBe(sessionId);
  expect(presented.browser.build).toHaveProperty("git_sha");
  expect(presented.coord?.build).toHaveProperty("git_sha");
  expect(presented.worker.worker_fp).toBe(fixtureWorker.workerFp);
  expect(presented.worker.build).toHaveProperty("git_sha");

  // Preserve the painted pane, commit its ordinary withdrawal, advance the
  // hidden PTY, then reject exactly its next positive reclaim. No socket,
  // visibility, resize, input, or reload edge follows the injected result:
  // the viewport owner must retry monotonically on this same healthy Sync
  // generation and the repair snapshot must reveal the hidden output.
  const otherSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, otherSessionId);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    return {
      coordSubscriptions: activeCoordSubscriptionCount(probe),
      coordViewers: coordViewerCount(probe),
      workerClaims: workerViewerClaimCount(probe),
    };
  }, { timeout: 10_000, intervals: [100] }).toEqual({
    coordSubscriptions: 0,
    coordViewers: 0,
    workerClaims: 0,
  });
  const retryRepairMarker = `RECLAIM-REPAIR:${suffix}`;
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: retryRepairMarker }),
  );
  expect((await attemptPaintedMarker(smokePage, sessionId, retryRepairMarker, 250)).proof)
    .toBeNull();
  expect(await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.rejectedViewportClaimCount(id);
  }, sessionId)).toBe(0);
  await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    smokeWindow.__smoke.rejectNextViewportClaim(id);
  }, sessionId);
  await switchToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.rejectedViewportClaimCount(id);
  }, sessionId), { timeout: 10_000, intervals: [50] }).toBe(1);
  const persistedPaint = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: overwriteMarker });
  expect(persistedPaint).toMatchObject({ marker: overwriteMarker, frames: 2 });

  const firstRejected = await readTerminalStreamProbe(smokePage, sessionId);
  const presentedDesired = presented.browser.claim.desired;
  const rejectedDesired = firstRejected.browser.claim.desired;
  if (!presentedDesired || !rejectedDesired) {
    throw new Error("reclaim sequence missing from terminal stream probe");
  }
  expect(BigInt(rejectedDesired.client_seq) > BigInt(presentedDesired.client_seq)).toBe(true);
  const repairedPaint = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 15_000);
  }, { id: sessionId, marker: retryRepairMarker });
  expect(repairedPaint).toMatchObject({ marker: retryRepairMarker, frames: 2 });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(smokePage, sessionId);
    const desired = probe.browser.claim.desired;
    const confirmed = probe.browser.claim.confirmed;
    return {
      retried: desired !== null
        && BigInt(desired.client_seq) > BigInt(rejectedDesired.client_seq),
      converged: desired !== null && confirmed?.client_seq === desired.client_seq,
      coordSubscriptions: activeCoordSubscriptionCount(probe),
      coordViewers: coordViewerCount(probe),
      workerClaims: workerViewerClaimCount(probe),
    };
  }, { timeout: 15_000, intervals: [50, 100, 250] }).toEqual({
    retried: true,
    converged: true,
    coordSubscriptions: 1,
    coordViewers: 1,
    workerClaims: 1,
  });
  expect(await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.rejectedViewportClaimCount(id);
  }, sessionId)).toBe(1);
});

test("terminal replay and Ctrl keys stay owned by the PTY", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const composer = slot.getByTestId("mobile-chat-input");
  await expect(composer).toBeVisible();
  await expect(composer.getByTestId("chat-box")).toBeVisible();
  await expect(composer.getByTestId("chat-input")).not.toBeFocused();

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  const marker = `NATIVE_FOCUS_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  const sidebar = smokePage.getByTestId("sidebar-desktop");
  const collapsed = await sidebar.getAttribute("data-collapsed");
  await smokePage.keyboard.type("cat -v");
  await smokePage.keyboard.press("Enter");
  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("b");
  await smokePage.keyboard.press("f");
  await smokePage.keyboard.up("Control");

  await smokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });

  await smokePage.keyboard.down("Control");
  await smokePage.keyboard.press("k");
  await smokePage.keyboard.up("Control");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain("^B^F^K");
  await expect(sidebar).toHaveAttribute("data-collapsed", collapsed ?? "false");
  await expect(smokePage.getByTestId("sidebar-search")).toHaveCount(0);
  await expect(smokePage.getByTestId("command-palette")).toHaveCount(0);
  await smokePage.keyboard.press("Control+C");
});

test("desktop active terminal keeps a permanent reserved composer after send", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop composer contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const display = slot.getByTestId("terminal-display");
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const input = box.getByTestId("chat-input");
  const mic = box.getByTestId("mobile-voice-input");
  const send = box.getByTestId("chat-send");

  await expect(dock).toBeVisible();
  await expect(display).toBeVisible();
  await expect(box).toBeVisible();
  await expect(input).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(mic).toBeVisible();
  await expect(mic.getByTestId("voice-mic")).toBeVisible();
  await expect(send).toBeVisible();

  // The only voice control is the inline mic in the permanent bar. Desktop no
  // longer exposes the old corner toggle, standalone voice FAB, or nav pad.
  await expect(smokePage.getByTestId("mobile-voice-input")).toHaveCount(1);
  await expect(smokePage.getByTestId("voice-mic")).toHaveCount(1);
  await expect(smokePage.getByTestId("terminal-chat-toggle")).toHaveCount(0);
  await expect(smokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(smokePage.getByTestId("terminal-nav-buttons")).toHaveCount(0);

  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  const readGeometry = () => smokePage.evaluate((id) => {
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector('[data-testid="terminal-display"]');
    const composer = slot?.querySelector('[data-testid="mobile-chat-input"]');
    if (!(terminal instanceof HTMLElement) || !(composer instanceof HTMLElement)) return null;

    const terminalRect = terminal.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    const sidebar = document.querySelector('[data-testid="sidebar-desktop"]');
    const sidebarRect = sidebar?.getBoundingClientRect();
    const viewportBottom = window.visualViewport
      ? window.visualViewport.offsetTop + window.visualViewport.height
      : window.innerHeight;
    return {
      terminalBottom: terminalRect.bottom,
      terminalHeight: terminalRect.height,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
      bottomGap: viewportBottom - composerRect.bottom,
      composerLeft: composerRect.left,
      sidebarRight: sidebarRect?.right ?? 0,
    };
  }, sessionId);

  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry !== null
      && geometry.terminalHeight > 0
      && geometry.composerHeight > 0
      && geometry.terminalBottom <= geometry.composerTop;
  }, { message: "desktop terminal content must end above the composer dock" }).toBe(true);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? Math.abs(geometry.bottomGap - 8) : Number.POSITIVE_INFINITY;
  }, { message: "desktop composer must rest about 8px above the viewport bottom" }).toBeLessThanOrEqual(4);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? geometry.composerLeft - geometry.sidebarRight : Number.NEGATIVE_INFINITY;
  }, { message: "desktop composer must stay inside the terminal pane" }).toBeGreaterThanOrEqual(4);

  const marker = `DESKTOP_COMPOSER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await input.fill(`printf '%s\\n' ${marker}`);
  await expect(input).toBeFocused();
  await send.click();
  await expect.poll(() => slot.textContent()).toContain(marker);
  await expect(input).toHaveValue("");
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(box).toHaveCount(1);
});

test("desktop split panes each own and route their composer", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop pane composer contract");
  const initialSessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, initialSessionId);

  await pressPlatformShortcut(smokePage, "splitRight", "D");

  const paneSlots = smokePage.locator("[data-pane-slot]");
  await expect(paneSlots).toHaveCount(2);
  await expect(paneSlots.nth(0)).toBeVisible();
  await expect(paneSlots.nth(1)).toBeVisible();

  const composers = smokePage.getByTestId("mobile-chat-input");
  await expect(composers).toHaveCount(2);
  await expect(composers.nth(0)).toBeVisible();
  await expect(composers.nth(1)).toBeVisible();

  const sessionIds = await paneSlots.evaluateAll((slots) => slots.map((slot) => {
    const testId = slot.getAttribute("data-testid") ?? "";
    return testId.startsWith("terminal-slot-")
      ? testId.slice("terminal-slot-".length)
      : "";
  }));
  expect(sessionIds).toHaveLength(2);
  expect(sessionIds).toContain(initialSessionId);
  expect(sessionIds.every(Boolean)).toBe(true);
  expect(new Set(sessionIds).size).toBe(2);

  const sessionA = sessionIds[0]!;
  const sessionB = sessionIds[1]!;
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  const slotB = smokePage.getByTestId(`terminal-slot-${sessionB}`);
  const dockA = slotA.getByTestId("mobile-chat-input");
  const dockB = slotB.getByTestId("mobile-chat-input");
  const inputA = dockA.getByTestId("chat-input");
  const inputB = dockB.getByTestId("chat-input");
  const sendA = dockA.getByTestId("chat-send");
  const sendB = dockB.getByTestId("chat-send");

  await expect(dockA).toHaveAttribute("data-placement", "pane");
  await expect(dockB).toHaveAttribute("data-placement", "pane");
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  const suffix = crypto.randomUUID().replace(/-/g, "").slice(0, 8);
  const markerA = `SPLIT_COMPOSER_A_${suffix}`;
  const markerB = `SPLIT_COMPOSER_B_${suffix}`;

  await inputA.click();
  await expect(inputA).toBeFocused();
  await expect(slotA).toHaveAttribute("data-focused", "true");
  await inputA.fill(`printf '%s\\n' ${markerA}`);
  await sendA.click();
  await expect.poll(() => slotA.textContent()).toContain(markerA);
  await expect(slotB).not.toContainText(markerA);
  await expect(slotA).not.toContainText(markerB);
  await expect(slotB).not.toContainText(markerB);
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  await inputB.click();
  await expect(inputB).toBeFocused();
  await expect(slotB).toHaveAttribute("data-focused", "true");
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();
  await inputB.fill(`printf '%s\\n' ${markerB}`);
  await sendB.click();
  await expect.poll(() => slotB.textContent()).toContain(markerB);
  await expect(slotA).not.toContainText(markerB);
  await expect(slotB).not.toContainText(markerA);
  await expect(dockA).toBeVisible();
  await expect(dockB).toBeVisible();

  const focusSlotABox = await slotA.boundingBox();
  const focusSlotBBox = await slotB.boundingBox();
  expect(focusSlotABox).not.toBeNull();
  expect(focusSlotBBox).not.toBeNull();
  const targetSlot = focusSlotABox!.x < focusSlotBBox!.x ? slotB : slotA;
  const walkKey = focusSlotABox!.x < focusSlotBBox!.x
    ? "ArrowRight"
    : "ArrowLeft";

  // Keyboard pane navigation overrides a composer focused in the pane being
  // left. The unsent draft stays there; subsequent native input belongs to the
  // newly focused PTY.
  const heldDraft = `held draft ${suffix}`;
  await inputA.click();
  await inputA.fill(heldDraft);
  await pressPlatformShortcut(smokePage, "paneFocus", walkKey);
  await expect(targetSlot).toHaveAttribute("data-focused", "true");
  const focusedMarker = `SPLIT_COMPOSER_FOCUS_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${focusedMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => targetSlot.textContent()).toContain(focusedMarker);
  await expect(inputA).toHaveValue(heldDraft);
  await inputA.click();
  await inputA.fill("");

  // A pointer-only send claims before the deck's ancestor handler, but must
  // release again when focus remained in the hidden PTY textarea.
  await slotA.getByTestId("terminal-display").click();
  await sendA.click();
  await pressPlatformShortcut(smokePage, "paneFocus", walkKey);
  await expect(targetSlot).toHaveAttribute("data-focused", "true");
  const keyboardMarker = `SPLIT_COMPOSER_KEYBOARD_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${keyboardMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => targetSlot.textContent()).toContain(keyboardMarker);

  // Releasing outside an inactive pane's send control still ends its transient
  // pointer claim. The window-level pointer-end watcher sees the off-dock release.
  await slotA.getByTestId("terminal-display").click();
  const sendBBox = await sendB.boundingBox();
  const displayABox = await slotA.getByTestId("terminal-display").boundingBox();
  expect(sendBBox).not.toBeNull();
  expect(displayABox).not.toBeNull();
  await smokePage.mouse.move(
    sendBBox!.x + sendBBox!.width / 2,
    sendBBox!.y + sendBBox!.height / 2,
  );
  await smokePage.mouse.down();
  await smokePage.mouse.move(
    displayABox!.x + displayABox!.width / 2,
    displayABox!.y + displayABox!.height / 2,
    { steps: 4 },
  );
  await smokePage.mouse.up();
  await expect(slotB).toHaveAttribute("data-focused", "true");
  const dragMarker = `SPLIT_COMPOSER_DRAG_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${dragMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotB.textContent()).toContain(dragMarker);


  // A legal 10% divider position must reflow, not paint one session's buttons
  // into its neighbor where they would become wrong-session hit targets.
  const deckBox = await smokePage.getByTestId("terminal-deck").boundingBox();
  const divider = smokePage.locator('[data-testid^="pane-divider-"]').first();
  const dividerBox = await divider.boundingBox();
  expect(deckBox).not.toBeNull();
  expect(dividerBox).not.toBeNull();
  expect(await divider.getAttribute("data-dir")).toBe("row");
  await smokePage.mouse.move(
    dividerBox!.x + dividerBox!.width / 2,
    dividerBox!.y + dividerBox!.height / 2,
  );
  await smokePage.mouse.down();
  await smokePage.mouse.move(
    deckBox!.x + deckBox!.width * 0.1,
    dividerBox!.y + dividerBox!.height / 2,
    { steps: 6 },
  );
  await smokePage.mouse.up();

  const slotABox = await slotA.boundingBox();
  const slotBBox = await slotB.boundingBox();
  expect(slotABox).not.toBeNull();
  expect(slotBBox).not.toBeNull();
  const narrowIsA = slotABox!.width < slotBBox!.width;
  const narrowSlot = narrowIsA ? slotA : slotB;
  const wideSlot = narrowIsA ? slotB : slotA;
  const narrowDock = narrowSlot.getByTestId("mobile-chat-input");
  const escapedControls = await narrowDock.evaluate((dock) => {
    const slot = dock.closest("[data-pane-slot]");
    if (!(slot instanceof HTMLElement)) return ["missing pane slot"];
    const bounds = slot.getBoundingClientRect();
    const controls = dock.querySelectorAll(
      '[data-testid="chat-attach"], [data-testid="chat-input"], [data-testid="voice-mic"], [data-testid="chat-send"]',
    );
    return Array.from(controls).flatMap((control) => {
      const rect = control.getBoundingClientRect();
      return rect.left < bounds.left - 1 || rect.right > bounds.right + 1
        ? [control.getAttribute("data-testid") ?? control.tagName]
        : [];
    });
  });
  expect(escapedControls).toEqual([]);

  const narrowInput = narrowDock.getByTestId("chat-input");
  const markerNarrow = `SPLIT_COMPOSER_NARROW_${suffix}`;
  await narrowInput.fill(`printf '%s\\n' ${markerNarrow}`);
  await narrowDock.getByTestId("chat-send").click();
  await expect.poll(() => narrowSlot.textContent()).toContain(markerNarrow);

  // A keyboard new-tab command overrides the focused composer but preserves its
  // parked draft; native keystrokes must belong to the spawned terminal.
  const parkedDraft = `parked new-tab draft ${suffix}`;
  await wideSlot.getByTestId("terminal-display").click();
  await expect(wideSlot).toHaveAttribute("data-focused", "true");
  await narrowInput.evaluate((input) => input.focus());
  await narrowInput.fill(parkedDraft);
  await expect(narrowSlot).toHaveAttribute("data-focused", "true");
  await pressPlatformShortcut(smokePage, "newTerminal", "T");
  await expect.poll(() => paneSlots.evaluateAll((slots) => slots.map((slot) =>
    (slot.getAttribute("data-testid") ?? "").replace(/^terminal-slot-/, ""),
  ).filter(Boolean).length)).toBe(3);
  const spawnedSessionId = await paneSlots.evaluateAll((slots, existingIds) =>
    slots.map((slot) =>
      (slot.getAttribute("data-testid") ?? "").replace(/^terminal-slot-/, ""),
    ).find((id) => id && !existingIds.includes(id)),
  [sessionA, sessionB]);
  expect(spawnedSessionId).toBeTruthy();
  const spawnedSlot = smokePage.getByTestId(`terminal-slot-${spawnedSessionId!}`);
  await expect(spawnedSlot).toHaveAttribute("data-focused", "true");
  const spawnedMarker = `SPLIT_COMPOSER_NEWTAB_${suffix}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${spawnedMarker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => spawnedSlot.textContent()).toContain(spawnedMarker);
  await expect(narrowInput).toHaveValue(parkedDraft);
});

test("desktop composer attaches exact files in order without submitting", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop native attachment contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  const firstName = `attach-first-${suffix}.bin`;
  const secondName = `attach-second-${suffix}.txt`;
  const firstBytes = Buffer.alloc(4 * 1024 * 1024 + 31);
  for (let i = 0; i < firstBytes.length; i++) firstBytes[i] = (i * 17 + suffix.charCodeAt(i % suffix.length)) & 0xff;
  const secondBytes = Buffer.from(`second-${suffix}\nline-two\0tail`, "utf8");

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const attach = box.getByTestId("chat-attach");
  const input = box.getByTestId("chat-input");
  await expect(attach).toBeVisible();
  await expect(attach).toHaveAttribute("aria-label", "Attach files");
  const [attachBox, inputBox] = await Promise.all([attach.boundingBox(), input.boundingBox()]);
  expect(attachBox).not.toBeNull();
  expect(inputBox).not.toBeNull();
  expect(attachBox!.x + attachBox!.width).toBeLessThanOrEqual(inputBox!.x);

  await input.fill("draft remains untouched");
  await expect(input).toBeFocused();
  const [slotBefore, dockBefore] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotBefore).not.toBeNull();
  expect(dockBefore).not.toBeNull();

  await resetTerminalInputCapture(smokePage);

  const [chooser] = await Promise.all([
    smokePage.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles([
    { name: firstName, mimeType: "application/octet-stream", buffer: firstBytes },
    { name: secondName, mimeType: "text/plain", buffer: secondBytes },
  ]);

  await expect(input).toBeFocused();
  await expect(input).toHaveValue("draft remains untouched");
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return [firstName, secondName].every((name) => response.entries.some((entry) => entry.filename === name));
  }, { timeout: 20_000 }).toBe(true);

  const entries = (await stack.client.listAttachments({ sessionId })).entries;
  const firstEntry = entries.find((entry) => entry.filename === firstName);
  const secondEntry = entries.find((entry) => entry.filename === secondName);
  expect(firstEntry).toBeDefined();
  expect(secondEntry).toBeDefined();
  expect(firstEntry!.sizeBytes).toBe(BigInt(firstBytes.length));
  expect(secondEntry!.sizeBytes).toBe(BigInt(secondBytes.length));

  const [storedFirst, storedSecond] = await Promise.all([
    readWorkerBytes(stack.client, stack.workerFp, firstEntry!.absPath),
    readWorkerBytes(stack.client, stack.workerFp, secondEntry!.absPath),
  ]);
  expect(storedFirst).toEqual(new Uint8Array(firstBytes));
  expect(storedSecond).toEqual(new Uint8Array(secondBytes));

  const expectedInput = `${firstEntry!.absPath} ${secondEntry!.absPath} `;
  const expectedBytes = Array.from(new TextEncoder().encode(expectedInput));
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(smokePage);
    return capture.batches.reduce((total, batch) => total + batch.data.length, 0);
  }).toBe(expectedBytes.length);
  const inputCapture = await readTerminalInputCapture(smokePage);
  expect(inputCapture.droppedBatches).toBe(0);
  expect(inputCapture.batches.every((batch) => batch.sessionId === sessionId)).toBe(true);
  const injected = inputCapture.batches.flatMap((batch) => batch.data);
  expect(injected).toEqual(expectedBytes);
  expect(new TextDecoder().decode(Uint8Array.from(injected))).toBe(expectedInput);
  expect(injected).not.toContain(13);
  expect(injected).not.toContain(10);

  const [slotAfter, dockAfter] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotAfter).not.toBeNull();
  expect(dockAfter).not.toBeNull();
  for (const key of ["x", "y", "width", "height"] as const) {
    expect(Math.abs(slotAfter![key] - slotBefore![key]), `terminal ${key}`).toBeLessThanOrEqual(1);
    expect(Math.abs(dockAfter![key] - dockBefore![key]), `composer ${key}`).toBeLessThanOrEqual(1);
  }
});

test("desktop composer submits Enter and grows above a stable terminal deck", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop composer keyboard and geometry contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const deck = smokePage.getByTestId("terminal-deck");
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const input = dock.getByTestId("chat-input");
  await expect(deck).toBeVisible();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(input).toBeVisible();
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });

  const readGeometry = () => smokePage.evaluate((id) => {
    const deckEl = document.querySelector('[data-testid="terminal-deck"]');
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminalEl = slotEl?.querySelector('[data-testid="terminal-display"]');
    const composerEl = slotEl?.querySelector('[data-testid="mobile-chat-input"]');
    if (
      !(deckEl instanceof HTMLElement)
      || !(slotEl instanceof HTMLElement)
      || !(terminalEl instanceof HTMLElement)
      || !(composerEl instanceof HTMLElement)
    ) return null;

    const slotRect = slotEl.getBoundingClientRect();
    const terminalRect = terminalEl.getBoundingClientRect();
    const composerRect = composerEl.getBoundingClientRect();
    return {
      deckClientHeight: deckEl.clientHeight,
      slotHeight: slotRect.height,
      terminalHeight: terminalRect.height,
      terminalBottom: terminalRect.bottom,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
    };
  }, sessionId);

  await input.fill("one-row draft");
  await expect(input).toBeFocused();
  const baseline = await readGeometry();
  if (!baseline) throw new Error("desktop composer geometry was unavailable at one row");
  expect(baseline.deckClientHeight).toBeGreaterThan(0);
  expect(baseline.slotHeight).toBeGreaterThan(0);
  expect(baseline.composerHeight).toBeGreaterThan(0);
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim;
    return claim.desired !== null && claim.confirmed?.client_seq === claim.desired.client_seq;
  }, { timeout: 10_000, intervals: [50] }).toBe(true);
  const baselineClaim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim.desired;
  if (!baselineClaim) throw new Error("desktop terminal omitted its baseline viewport claim");

  const growthDraft = [
    "first composer row",
    "second composer row",
    "third composer row",
    "fourth composer row",
    "fifth composer row",
  ].join("\n");
  await input.fill(growthDraft);
  await expect(input).toHaveValue(growthDraft);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry && geometry.composerHeight > baseline.composerHeight + 1
      ? geometry.terminalBottom - geometry.composerTop
      : Number.POSITIVE_INFINITY;
  }, {
    message: "grown desktop composer must move terminal content above its top",
  }).toBeLessThanOrEqual(0);

  const grown = await readGeometry();
  if (!grown) throw new Error("desktop composer geometry was unavailable after growth");
  expect(grown.composerHeight).toBeGreaterThan(baseline.composerHeight + 1);
  expect(
    Math.abs(grown.deckClientHeight - baseline.deckClientHeight),
    "composer growth must not resize TerminalDeck",
  ).toBeLessThanOrEqual(1);
  expect(
    Math.abs(grown.slotHeight - baseline.slotHeight),
    "composer growth must not resize the terminal slot",
  ).toBeLessThanOrEqual(1);
  expect(grown.terminalBottom, "terminal visual bottom must stay at or above the composer").toBeLessThanOrEqual(
    grown.composerTop,
  );
  expect(
    baseline.terminalHeight - grown.terminalHeight,
    "desktop composer autogrow must resize terminal-display",
  ).toBeGreaterThan(1);
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim;
    return claim.desired !== null
      && claim.confirmed?.client_seq === claim.desired.client_seq
      && BigInt(claim.desired.client_seq) > BigInt(baselineClaim.client_seq)
      && claim.desired.cols === baselineClaim.cols
      && claim.desired.rows < baselineClaim.rows;
  }, {
    message: "desktop terminal-display resize must produce one debounced adopted claim",
    timeout: 10_000,
    intervals: [50],
  }).toBe(true);
  const grownClaim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim.desired;
  if (!grownClaim) throw new Error("desktop terminal omitted its grown viewport claim");
  expect(BigInt(grownClaim.client_seq) > BigInt(baselineClaim.client_seq)).toBe(true);
  expect(grownClaim.rows).toBeLessThan(baselineClaim.rows);

  const growthOutputMarker = `DESKTOP_GROWTH_LIVE_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  await inputSmokeTerminal(smokePage, sessionId, `printf '%s\\n' ${growthOutputMarker}\r`);
  const growthOutputProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: growthOutputMarker });
  expect(growthOutputProof).toMatchObject({ marker: growthOutputMarker, frames: 2 });
  const afterGrowthOutput = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterGrowthOutput.browser.handler_canonical).toEqual(afterGrowthOutput.browser.dom_reconciled);
  expect(afterGrowthOutput.browser.presentation?.reader_intent).toBe("live");

  const shiftMarker = `DESKTOP_SHIFT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  const shiftCommand = `printf '%s\\n' ${shiftMarker}`;
  await input.fill(shiftCommand);
  await input.press("Shift+Enter");
  await expect(input).toHaveValue(`${shiftCommand}\n`);

  // A command sent directly after Shift+Enter is a PTY ordering barrier: once
  // it is visible, a mistakenly submitted Shift+Enter command would be visible too.
  const barrier = `DESKTOP_SHIFT_BARRIER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await inputSmokeTerminal(smokePage, sessionId, `printf '%s\\n' ${barrier}\n`);
  const barrierProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: barrier });
  expect(barrierProof).toMatchObject({ marker: barrier, frames: 2 });
  expect((await attemptPaintedMarker(smokePage, sessionId, shiftMarker, 300)).proof).toBeNull();

  const enterMarker = `DESKTOP_ENTER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await input.fill(`printf '%s\\n' ${enterMarker}`);
  await expect(input).toBeFocused();
  await input.press("Enter");
  const enterProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: enterMarker });
  expect(enterProof).toMatchObject({ marker: enterMarker, frames: 2 });
  await expect(input).toHaveValue("");
  await expect(input).toBeFocused();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(box).toHaveCount(1);
});

test("mobile composer starts unfocused and reserves terminal space through rotation", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const dock = mobileSmokePage.getByTestId("mobile-chat-input");
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(dock).toBeVisible();
  await expect(input).toBeVisible();
  await expect(input).not.toBeFocused();
  await mobileSmokePage.getByTestId("chat-box")
    .evaluate(async (el) => { await Promise.all(el.getAnimations().map((animation) => animation.finished)); });

  const initialViewport = mobileSmokePage.viewportSize();
  expect(initialViewport).not.toBeNull();

  const readGeometry = () => mobileSmokePage.evaluate((id) => {
    const terminal = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const composer = document.querySelector('[data-testid="mobile-chat-input"]');
    if (!(terminal instanceof HTMLElement) || !(composer instanceof HTMLElement)) return null;

    const safeAreaProbe = document.createElement("div");
    safeAreaProbe.style.cssText = [
      "position:fixed",
      "visibility:hidden",
      "pointer-events:none",
      "padding-bottom:env(safe-area-inset-bottom, 0px)",
    ].join(";");
    document.body.append(safeAreaProbe);
    const measuredSafeArea = Number.parseFloat(getComputedStyle(safeAreaProbe).paddingBottom);
    safeAreaProbe.remove();

    const terminalRect = terminal.getBoundingClientRect();
    const composerRect = composer.getBoundingClientRect();
    const keyboardOffset = Number.parseFloat(
      getComputedStyle(document.documentElement).getPropertyValue("--kb-offset"),
    );
    const viewportBottom = window.visualViewport
      ? window.visualViewport.offsetTop + window.visualViewport.height
      : window.innerHeight;
    return {
      terminalBottom: terminalRect.bottom,
      terminalHeight: terminalRect.height,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
      bottomGap: viewportBottom - composerRect.bottom,
      safeAreaBottom: Number.isFinite(measuredSafeArea) ? measuredSafeArea : 0,
      keyboardOffset: Number.isFinite(keyboardOffset) ? keyboardOffset : 0,
    };
  }, sessionId);

  const expectReservedGeometry = async (label: string) => {
    await expect.poll(async () => {
      const geometry = await readGeometry();
      if (!geometry || geometry.terminalHeight <= 0 || geometry.composerHeight <= 0) {
        return Number.POSITIVE_INFINITY;
      }
      return Math.abs(geometry.terminalBottom - geometry.composerTop);
    }, { message: `${label}: terminal bottom must meet the composer top` }).toBeLessThanOrEqual(2);
    await expect.poll(async () => (await readGeometry())?.keyboardOffset ?? Number.POSITIVE_INFINITY, {
      message: `${label}: keyboard offset must clear without a simulated keyboard`,
    }).toBe(0);
    await expect.poll(async () => {
      const geometry = await readGeometry();
      return geometry
        ? Math.abs(geometry.bottomGap - geometry.safeAreaBottom - 8)
        : Number.POSITIVE_INFINITY;
    }, {
      message: `${label}: resting dock must sit about 8px above the measured safe area`,
    }).toBeLessThanOrEqual(4);
    const geometry = await readGeometry();
    expect(geometry, `${label}: visible terminal and composer geometry`).not.toBeNull();
    return geometry!;
  };

  const initial = await expectReservedGeometry("initial portrait");
  const portrait = initialViewport!;
  await mobileSmokePage.setViewportSize({ width: portrait.height, height: portrait.width });
  await expectReservedGeometry("landscape");
  await mobileSmokePage.setViewportSize(portrait);
  await expectReservedGeometry("restored portrait");

  // Crossing the real compact boundary swaps the body portal for this pane's
  // inline instance. Instance-token cleanup must not clear the replacement's
  // focus owner or leave the viewport reserve stuck on desktop.
  await mobileSmokePage.setViewportSize({ width: 1024, height: 700 });
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  const paneDock = slot.getByTestId("mobile-chat-input");
  await expect(paneDock).toBeVisible();
  await expect(paneDock).toHaveAttribute("data-placement", "pane");
  await expect.poll(() => paneDock.evaluate((el) => getComputedStyle(el).position)).toBe("relative");
  await mobileSmokePage.setViewportSize(portrait);
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveAttribute("data-placement", "viewport");
  await expectReservedGeometry("portrait after desktop handoff");
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry ? Math.abs(geometry.terminalHeight - initial.terminalHeight) : Number.POSITIVE_INFINITY;
  }, { message: "portrait terminal height must recover after rotation" }).toBeLessThanOrEqual(2);

  await expect(input).not.toBeFocused();
  await input.tap();
  await expect(input).toBeFocused();
});

// Compact composition is structurally two rows from its first paint. The field
// always owns the full first row; its content never decides whether the actions
// consume message width. Explicit lines and ordinary wrapping therefore share
// one stable wrap width while only the top of the fixed dock moves.
test("mobile composer gives multiline drafts the full message width", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");

  const readComposerGeometry = () => mobileSmokePage.evaluate((id) => {
    const dockEl = document.querySelector('[data-testid="mobile-chat-input"]');
    const boxEl = document.querySelector('[data-testid="chat-box"]');
    const inputEl = document.querySelector('[data-testid="chat-input"]');
    const micEl = document.querySelector('[data-testid="voice-mic"]');
    const sendEl = document.querySelector('[data-testid="chat-send"]');
    const deckEl = document.querySelector('[data-testid="terminal-deck"]');
    const terminalEl = document.querySelector(
      `[data-testid="terminal-slot-${id}"] [data-testid="terminal-display"]`,
    );
    if (
      !(dockEl instanceof HTMLElement)
      || !(boxEl instanceof HTMLElement)
      || !(inputEl instanceof HTMLTextAreaElement)
      || !(micEl instanceof HTMLElement)
      || !(sendEl instanceof HTMLElement)
      || !(deckEl instanceof HTMLElement)
      || !(terminalEl instanceof HTMLElement)
    ) return null;

    const rect = (el: HTMLElement) => {
      const value = el.getBoundingClientRect();
      return {
        x: value.x,
        y: value.y,
        top: value.top,
        right: value.right,
        bottom: value.bottom,
        left: value.left,
        width: value.width,
        height: value.height,
      };
    };
    const px = (value: string) => {
      const parsed = Number.parseFloat(value);
      return Number.isFinite(parsed) ? parsed : 0;
    };
    const resolveMaxHeight = (value: string) => {
      const probe = document.createElement("div");
      probe.style.cssText = [
        "position:fixed",
        "visibility:hidden",
        "pointer-events:none",
        "box-sizing:border-box",
        "height:10000px",
      ].join(";");
      probe.style.maxHeight = value;
      document.body.append(probe);
      const height = probe.getBoundingClientRect().height;
      probe.remove();
      return height;
    };

    const boxRect = rect(boxEl);
    const boxStyle = getComputedStyle(boxEl);
    const inputStyle = getComputedStyle(inputEl);
    const paddingLeft = px(boxStyle.paddingLeft);
    const paddingRight = px(boxStyle.paddingRight);
    return {
      dock: rect(dockEl),
      box: boxRect,
      input: rect(inputEl),
      mic: rect(micEl),
      send: rect(sendEl),
      deck: rect(deckEl),
      deckOffsetHeight: deckEl.offsetHeight,
      deckTransform: getComputedStyle(deckEl).transform,
      terminal: rect(terminalEl),
      terminalClientHeight: terminalEl.clientHeight,
      innerLeft: boxRect.left + paddingLeft,
      innerRight: boxRect.right - paddingRight,
      paddingTop: px(boxStyle.paddingTop),
      paddingBottom: px(boxStyle.paddingBottom),
      rowGap: px(boxStyle.rowGap),
      columnGap: px(boxStyle.columnGap),
      inputClientHeight: inputEl.clientHeight,
      inputScrollHeight: inputEl.scrollHeight,
      lineHeight: px(inputStyle.lineHeight),
      overflowY: inputStyle.overflowY,
      maxHeight: resolveMaxHeight(inputStyle.maxHeight),
      contractMaxHeight: resolveMaxHeight("min(192px, 30dvh)"),
    };
  }, sessionId);

  const geometry = async () => {
    const value = await readComposerGeometry();
    expect(value, "compact composer geometry").not.toBeNull();
    return value!;
  };
  const near = (actual: number, expected: number, tolerance: number, label: string) =>
    expect(Math.abs(actual - expected), label).toBeLessThanOrEqual(tolerance);

  // The entrance animation translates the pill. Finish it before recording the
  // fixed footer coordinates used throughout the growth assertions.
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  await input.fill("short");
  const baseline = await geometry();
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(mobileSmokePage, sessionId)).browser.claim;
    return claim.desired !== null && claim.confirmed?.client_seq === claim.desired.client_seq;
  }, { timeout: 10_000, intervals: [50] }).toBe(true);
  const baselineClaim = (await readTerminalStreamProbe(mobileSmokePage, sessionId)).browser.claim.desired;
  if (!baselineClaim) throw new Error("compact terminal omitted its baseline viewport claim");

  const expectFullWidthRows = (
    current: typeof baseline,
    label: string,
  ) => {
    const innerWidth = current.innerRight - current.innerLeft;
    near(current.input.left, current.innerLeft, 1, `${label}: textarea left`);
    near(current.input.width, innerWidth, 1, `${label}: textarea uses the box inner width`);
    near(current.input.right, current.innerRight, 1, `${label}: textarea right`);
    expect(
      current.input.right - current.mic.left,
      `${label}: textarea continues across the mic and send columns`,
    ).toBeGreaterThanOrEqual(current.mic.width + current.send.width - 2);
    expect(
      current.input.right - current.send.left,
      `${label}: textarea continues across the send column`,
    ).toBeGreaterThanOrEqual(current.send.width - 2);

    near(current.mic.top, current.send.top, 1, `${label}: action row top`);
    near(current.mic.bottom, current.send.bottom, 1, `${label}: action row bottom`);
    near(current.send.right, current.innerRight, 1, `${label}: footer is right-aligned`);
    near(
      current.send.left - current.mic.right,
      current.columnGap,
      1,
      `${label}: action gap`,
    );
    near(
      current.mic.top - current.input.bottom,
      current.rowGap,
      1,
      `${label}: textarea-to-footer gap`,
    );
    expect(current.mic.top, `${label}: mic is below the textarea`).toBeGreaterThan(
      current.input.bottom,
    );
  };
  const expectStableFooter = (
    current: typeof baseline,
    label: string,
  ) => {
    for (const control of ["mic", "send"] as const) {
      for (const dimension of ["x", "y", "width", "height"] as const) {
        near(
          current[control][dimension],
          baseline[control][dimension],
          1,
          `${label}: stable ${control}.${dimension}`,
        );
      }
    }
    near(current.box.left, baseline.box.left, 1, `${label}: stable box left`);
    near(current.box.width, baseline.box.width, 1, `${label}: stable box width`);
    near(current.input.left, baseline.input.left, 1, `${label}: stable textarea left`);
    near(current.input.width, baseline.input.width, 1, `${label}: stable textarea width`);
  };

  expectFullWidthRows(baseline, "resting");
  near(baseline.input.height, 40, 1, "resting textarea height");
  near(baseline.mic.height, 44, 1, "resting mic height");
  near(baseline.send.height, 44, 1, "resting send height");
  near(baseline.rowGap, 4, 0.5, "resting row gap");
  near(baseline.paddingTop + baseline.paddingBottom, 8, 1, "resting outer padding");
  near(baseline.box.height, 96, 1, "settled compact box height");
  near(baseline.dock.height, 96, 1, "settled compact dock height");

  const explicitLines = [
    "first explicit line",
    "second explicit line",
    "third explicit line",
    "fourth explicit line",
  ].join("\n");
  await input.fill(explicitLines);
  await expect(input).toHaveValue(explicitLines);
  const multiline = await geometry();
  expectFullWidthRows(multiline, "explicit newlines");
  expectStableFooter(multiline, "explicit newlines");
  expect(multiline.input.height, "explicit newlines grow the textarea").toBeGreaterThan(
    baseline.input.height + multiline.lineHeight,
  );
  expect(multiline.input.height, "explicit newlines remain below the cap").toBeLessThan(
    multiline.maxHeight - 1,
  );
  expect(multiline.dock.height, "explicit newlines grow the dock").toBeGreaterThan(
    baseline.dock.height + 1,
  );
  expect(multiline.dock.top, "the growing dock moves upward").toBeLessThan(
    baseline.dock.top - 1,
  );
  near(multiline.dock.bottom, baseline.dock.bottom, 1, "explicit newlines: stable dock bottom");

  // Deliberately abundant prose guarantees overflow without choosing a string
  // near any wrap threshold; text width never selects a different layout.
  const wrappingDraft = (
    "ordinary wrapping keeps every line as wide as the message surface "
  ).repeat(80);
  await input.fill(wrappingDraft);
  await expect(input).toHaveValue(wrappingDraft);
  const capped = await geometry();
  expectFullWidthRows(capped, "long wrapping draft");
  expectStableFooter(capped, "long wrapping draft");
  near(capped.maxHeight, capped.contractMaxHeight, 1, "compact textarea CSS max-height");
  near(capped.input.height, capped.maxHeight, 1, "long draft is capped at CSS max-height");
  expect(
    capped.inputScrollHeight - capped.inputClientHeight,
    "capped textarea has internally scrollable overflow",
  ).toBeGreaterThan(capped.lineHeight);
  expect(capped.overflowY).toBe("auto");
  expect(capped.dock.height, "wrapped text grows the dock to its cap").toBeGreaterThan(
    multiline.dock.height + 1,
  );
  expect(capped.dock.top, "capped dock grows upward").toBeLessThan(multiline.dock.top - 1);
  near(capped.dock.bottom, baseline.dock.bottom, 1, "long wrapping draft: stable dock bottom");

  await expect.poll(async () => {
    const current = await readComposerGeometry();
    return current ? baseline.deck.top - current.deck.top : 0;
  }, {
    message: "compact composer growth must translate the deck without changing its layout box",
  }).toBeGreaterThan(1);
  const transformed = await geometry();
  near(transformed.deckOffsetHeight, baseline.deckOffsetHeight, 1, "compact deck layout height");
  near(transformed.deck.height, baseline.deck.height, 1, "compact deck painted height");
  near(transformed.terminalClientHeight, baseline.terminalClientHeight, 1, "compact terminal layout height");
  near(transformed.terminal.height, baseline.terminal.height, 1, "compact terminal painted height");
  expect(transformed.deck.top, "compact deck paint translates upward").toBeLessThan(baseline.deck.top - 1);
  near(
    baseline.terminal.top - transformed.terminal.top,
    baseline.deck.top - transformed.deck.top,
    1,
    "terminal and deck share the same compact paint transform",
  );
  expect(transformed.deckTransform).not.toBe(baseline.deckTransform);

  // Wait past the desktop ResizeObserver debounce. Compact growth is transform
  // only, so it must not enqueue a viewport claim even while output stays live.
  await mobileSmokePage.waitForTimeout(250);
  const compactClaim = (await readTerminalStreamProbe(mobileSmokePage, sessionId)).browser.claim;
  expect(compactClaim.desired).toMatchObject({
    client_seq: baselineClaim.client_seq,
    cols: baselineClaim.cols,
    rows: baselineClaim.rows,
  });
  expect(compactClaim.confirmed?.client_seq).toBe(baselineClaim.client_seq);
  const compactGrowthMarker = `COMPACT_GROWTH_LIVE_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  await inputSmokeTerminal(mobileSmokePage, sessionId, `printf '%s\\n' ${compactGrowthMarker}\r`);
  const compactGrowthProof = await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: compactGrowthMarker });
  expect(compactGrowthProof).toMatchObject({ marker: compactGrowthMarker, frames: 2 });
  const afterCompactGrowth = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  expect(afterCompactGrowth.browser.handler_canonical).toEqual(afterCompactGrowth.browser.dom_reconciled);
  expect(afterCompactGrowth.browser.presentation?.reader_intent).toBe("live");
  await expect(input).toHaveValue(wrappingDraft);

  await input.fill("short");
  await expect.poll(async () => Math.abs((await geometry()).deck.top - baseline.deck.top))
    .toBeLessThanOrEqual(1);
  const restored = await geometry();
  expectFullWidthRows(restored, "shortened draft");
  for (const surface of ["dock", "box", "input", "mic", "send"] as const) {
    for (const dimension of ["x", "y", "width", "height"] as const) {
      near(
        restored[surface][dimension],
        baseline[surface][dimension],
        1,
        `shortened draft restores ${surface}.${dimension}`,
      );
    }
  }
  for (const surface of ["deck", "terminal"] as const) {
    for (const dimension of ["top", "height"] as const) {
      near(
        restored[surface][dimension],
        baseline[surface][dimension],
        1,
        `shortened draft restores ${surface}.${dimension}`,
      );
    }
  }
});

// The permanent compact composer keeps its field and all three direct actions
// mounted. Outside taps, Escape, and attachment selection must retain its draft.
test("mobile composer is permanent field + attachment + mic + send", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  const fileName = `compact-attach-${suffix}.txt`;
  const fileBytes = Buffer.from(`compact attachment ${suffix}\n`, "utf8");
  const draft = `retained compact draft ${suffix}`;
  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  const attach = box.getByTestId("chat-attach");
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(box).toBeVisible();
  await expect(attach).toBeVisible();
  await expect(attach).toHaveAttribute("aria-label", "Attach files");
  await expect(box.getByTestId("mobile-voice-input")).toBeVisible();
  await expect(box.getByTestId("voice-mic")).toBeVisible();
  await expect(box.getByTestId("chat-send")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-add-menu")).toHaveCount(0);

  await input.fill(draft);
  await expect(input).toBeFocused();
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue(draft);

  await input.tap();
  await expect(input).toBeFocused();
  await input.press("Escape");
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue(draft);

  await resetTerminalInputCapture(mobileSmokePage);

  const [chooser] = await Promise.all([
    mobileSmokePage.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles([{ name: fileName, mimeType: "text/plain", buffer: fileBytes }]);

  await expect(input).toHaveValue(draft);
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return response.entries.some((entry) => entry.filename === fileName);
  }, { timeout: 20_000 }).toBe(true);

  const entry = (await stack.client.listAttachments({ sessionId })).entries.find(
    (candidate) => candidate.filename === fileName,
  );
  expect(entry).toBeDefined();
  expect(entry!.sizeBytes).toBe(BigInt(fileBytes.length));

  const expectedInput = Array.from(new TextEncoder().encode(`${entry!.absPath} `));
  await expect.poll(async () =>
    (await readTerminalInputCapture(mobileSmokePage)).batches.flatMap((batch) => batch.data)
  ).toEqual(expectedInput);
  const inputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(inputCapture.droppedBatches).toBe(0);
  expect(inputCapture.batches.every((batch) => batch.sessionId === sessionId)).toBe(true);
  expect(expectedInput).not.toContain(13);
  expect(expectedInput).not.toContain(10);
  await expect(input).toHaveValue(draft);
});

// The three compact actions share one flat 44dp M3 control anatomy.
test("inline composer controls share one compact M3 anatomy", async ({ mobileSmokePage, stack }) => {
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const box = mobileSmokePage.getByTestId("chat-box");
  const anatomy = (testId: string) => box.getByTestId(testId).evaluate((el) => {
    const style = getComputedStyle(el);
    return {
      width: style.width,
      height: style.height,
      borderRadius: style.borderRadius,
      boxShadow: style.boxShadow,
    };
  });

  await expect(box).toBeVisible();
  const [attach, mic, send] = await Promise.all([
    anatomy("chat-attach"),
    anatomy("voice-mic"),
    anatomy("chat-send"),
  ]);
  for (const [label, control] of [
    ["chat-attach", attach],
    ["voice-mic", mic],
    ["chat-send", send],
  ] as const) {
    expect(control.width, label).toBe("44px");
    expect(control.height, label).toBe("44px");
    expect(control.borderRadius, label).toBe(send.borderRadius);
    expect(control.boxShadow, label).toBe("none");
  }
});

test("mobile terminal keyboard toggles and dispatches special keys", async ({ mobileSmokePage, stack }) => {
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(mobileSmokePage, fixtureWorker);
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });
  await expectSmokeComposer(mobileSmokePage);

  const terminalSlot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = mobileSmokePage.getByTestId("mobile-chat-input");
  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  const toggle = mobileSmokePage.getByTestId("terminal-nav-toggle");
  const panel = mobileSmokePage.getByTestId("terminal-nav-buttons");
  const paneFocused = () => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId);

  expect(await box.evaluate((el) => Array.from(el.children).map((child) =>
    child instanceof HTMLElement ? child.dataset.testid ?? null : null,
  ))).toEqual(["chat-attach", "chat-input", "mobile-voice-input", "chat-send"]);
  await expect(toggle).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "false");
  await expect(toggle).toHaveAttribute("aria-label", "Show terminal keys");
  await expect(toggle).toHaveCSS("position", "fixed");
  await expect(toggle).toHaveCSS("width", "56px");
  await expect(toggle).toHaveCSS("height", "56px");
  await expect(panel).toHaveCount(0);
  await expect(dock.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(box.getByTestId("terminal-nav-toggle")).toHaveCount(0);

  const readGeometry = () => mobileSmokePage.evaluate((id) => {
    const terminal = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const composerDock = document.querySelector('[data-testid="mobile-chat-input"]');
    const chatBox = document.querySelector('[data-testid="chat-box"]');
    const keyToggle = document.querySelector('[data-testid="terminal-nav-toggle"]');
    const keyPanel = document.querySelector('[data-testid="terminal-nav-buttons"]');
    if (
      !(terminal instanceof HTMLElement)
      || !(composerDock instanceof HTMLElement)
      || !(chatBox instanceof HTMLElement)
      || !(keyToggle instanceof HTMLElement)
      || (keyPanel !== null && !(keyPanel instanceof HTMLElement))
    ) return null;

    const readRect = (element: HTMLElement) => {
      const rect = element.getBoundingClientRect();
      return {
        left: rect.left,
        top: rect.top,
        right: rect.right,
        bottom: rect.bottom,
        width: rect.width,
        height: rect.height,
      };
    };
    const terminalRect = readRect(terminal);
    const dockRect = readRect(composerDock);
    const chatRect = readRect(chatBox);
    const toggleRect = readRect(keyToggle);
    const panelRect = keyPanel ? readRect(keyPanel) : null;
    const viewport = window.visualViewport;
    const viewportLeft = viewport?.offsetLeft ?? 0;
    const viewportTop = viewport?.offsetTop ?? 0;
    const viewportRight = viewportLeft + (viewport?.width ?? window.innerWidth);
    const viewportBottom = viewportTop + (viewport?.height ?? window.innerHeight);
    const panelStyle = keyPanel ? getComputedStyle(keyPanel) : null;
    return {
      terminal: terminalRect,
      dock: dockRect,
      chat: chatRect,
      toggle: toggleRect,
      panel: panelRect,
      detached: !composerDock.contains(keyToggle)
        && !chatBox.contains(keyToggle)
        && (!keyPanel || (!composerDock.contains(keyPanel) && !chatBox.contains(keyPanel))),
      togglePosition: getComputedStyle(keyToggle).position,
      panelPosition: panelStyle?.position ?? null,
      panelAboveToggle: panelRect ? panelRect.bottom <= toggleRect.top + 1 : null,
      surfacesInside: [toggleRect, chatRect, ...(panelRect ? [panelRect] : [])].every((rect) =>
        rect.left >= viewportLeft - 1
        && rect.top >= viewportTop - 1
        && rect.right <= viewportRight + 1
        && rect.bottom <= viewportBottom + 1),
      panelOverflowY: panelStyle?.overflowY ?? null,
      panelScrollHeight: keyPanel?.scrollHeight ?? null,
      panelClientHeight: keyPanel?.clientHeight ?? null,
    };
  }, sessionId);
  const rectDelta = (
    actual: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    expected: { left: number; top: number; right: number; bottom: number; width: number; height: number },
  ) => Math.max(
    Math.abs(actual.left - expected.left),
    Math.abs(actual.top - expected.top),
    Math.abs(actual.right - expected.right),
    Math.abs(actual.bottom - expected.bottom),
    Math.abs(actual.width - expected.width),
    Math.abs(actual.height - expected.height),
  );
  const expectStableRect = (
    actual: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    expected: { left: number; top: number; right: number; bottom: number; width: number; height: number },
    label: string,
  ) => {
    const delta = rectDelta(actual, expected);
    expect(delta, label).toBeLessThanOrEqual(2);
  };

  const draft = "unsent navigation draft";
  await input.fill(draft);
  await mobileSmokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });
  const closedGeometry = await readGeometry();
  expect(closedGeometry, "closed floating keyboard geometry").not.toBeNull();
  expect(closedGeometry!.detached).toBe(true);
  expect(closedGeometry!.togglePosition).toBe("fixed");
  expect(closedGeometry!.surfacesInside).toBe(true);
  expect(closedGeometry!.toggle.width).toBeCloseTo(56, 0);
  expect(closedGeometry!.toggle.height).toBeCloseTo(56, 0);

  await toggle.tap();
  await expect(input).toHaveValue(draft);
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(toggle).toHaveAttribute("aria-label", "Hide terminal keys");
  await expect(panel).toBeVisible();
  await expect(panel).toHaveCSS("position", "fixed");
  await expect(dock.getByTestId("terminal-nav-buttons")).toHaveCount(0);
  await expect(box.getByTestId("terminal-nav-buttons")).toHaveCount(0);
  await panel.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });

  const expectOpenGeometry = async (label: string) => {
    await expect.poll(async () => {
      const geometry = await readGeometry();
      if (!geometry?.panel) return false;
      return geometry.detached
        && geometry.togglePosition === "fixed"
        && geometry.panelPosition === "fixed"
        && geometry.panelAboveToggle === true
        && geometry.surfacesInside;
    }, { message: `${label}: detached fixed panel and toggle must remain inside the viewport` }).toBe(true);
    const geometry = await readGeometry();
    expect(geometry?.panel, `${label}: open floating keyboard geometry`).not.toBeNull();
    return geometry!;
  };

  const initialPortrait = await expectOpenGeometry("initial portrait");
  expectStableRect(initialPortrait.dock, closedGeometry!.dock, "opening must not move or resize the composer dock");
  expectStableRect(initialPortrait.terminal, closedGeometry!.terminal, "opening must not change terminal geometry");

  const directKeys = [
    ["nav-esc", [0x1b]],
    ["nav-tab", [0x09]],
    ["nav-backspace", [0x7f]],
    ["nav-home", [0x1b, 0x5b, 0x48]],
    ["nav-up", [0x1b, 0x5b, 0x41]],
    ["nav-end", [0x1b, 0x5b, 0x46]],
    ["nav-pgup", [0x1b, 0x5b, 0x35, 0x7e]],
    ["nav-left", [0x1b, 0x5b, 0x44]],
    ["nav-down", [0x1b, 0x5b, 0x42]],
    ["nav-right", [0x1b, 0x5b, 0x43]],
    ["nav-pgdn", [0x1b, 0x5b, 0x36, 0x7e]],
    ["nav-enter", [0x0d]],
  ] as const;
  const expectedDirectBytes = directKeys.flatMap(([, data]) => [...data]);
  for (const [testId] of directKeys) await expect(mobileSmokePage.getByTestId(testId)).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("nav-mouse")).toBeVisible();

  // The key sheet is portaled outside the pane, but its accepted navigation
  // bytes still belong to this renderer. Passive fixture output stays pending
  // under the selection until nav-up synchronously resumes the pane.
  const navRecoveryMarker = `NAV-RECOVERY:${crypto.randomUUID().replaceAll("-", "").slice(0, 10)}`;
  const beforeNavPending = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  expect(await holdNativeTerminalSelection(mobileSmokePage, sessionId)).toBe(true);
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: navRecoveryMarker }),
  );
  const navPending = await waitForCanonicalAdvance(mobileSmokePage, sessionId, beforeNavPending);
  expectCanonicalAdvanceHeld(beforeNavPending, navPending, {
    readerReason: "selection",
    selectionHold: true,
  });
  const hiddenNavMarker = await attemptPaintedMarker(mobileSmokePage, sessionId, navRecoveryMarker);
  expect(hiddenNavMarker.proof).toBeNull();
  await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "click", {
    marker: navRecoveryMarker,
    targetTestId: "nav-up",
  });
  await mobileSmokePage.getByTestId("nav-up").tap();
  const immediateNavPaint = await readImmediateTerminalPaintSample(mobileSmokePage);
  expect(immediateNavPaint).toMatchObject({
    eventType: "click",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateNavPaint?.markerRowRect).not.toBeNull();
  const navRecoveredProof = await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: navRecoveryMarker });
  expect(navRecoveredProof).toMatchObject({ marker: navRecoveryMarker, frames: 2 });
  expectRecoveredLive(navPending, await readTerminalStreamProbe(mobileSmokePage, sessionId));
  await expect(input).toHaveValue(draft);

  await resetTerminalInputCapture(mobileSmokePage);

  await mobileSmokePage.evaluate(() => {
    document.body.tabIndex = -1;
    document.body.focus();
  });
  expect(await mobileSmokePage.evaluate(() => document.activeElement === document.body)).toBe(true);
  await expect.poll(paneFocused).toBe(false);
  for (const [testId] of directKeys) {
    await mobileSmokePage.getByTestId(testId).tap();
    expect(await paneFocused(), `${testId} must not focus the hidden terminal textarea`).toBe(false);
  }
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(mobileSmokePage);
    return capture.batches.reduce((total, inputBatch) => total + inputBatch.data.length, 0);
  }).toBe(expectedDirectBytes.length);
  const directInputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(directInputCapture.droppedBatches).toBe(0);
  expect(directInputCapture.batches.every((inputBatch) => inputBatch.sessionId === sessionId)).toBe(true);
  expect(directInputCapture.batches.flatMap((inputBatch) => inputBatch.data)).toEqual(expectedDirectBytes);

  // Forwarding defaults ON now that the gate is the application's own DECSET
  // request: an app that never asked for the mouse never receives events either
  // way, so an opt-in only cost mouse-aware TUIs their mouse. The toggle survives
  // as the reverse escape hatch and is left ON for the forwarding cases below.
  const mouse = mobileSmokePage.getByTestId("nav-mouse");
  await expect(mouse).toHaveAttribute("aria-pressed", "true");
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "false");
  expect(await paneFocused()).toBe(false);
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "true");
  expect(await paneFocused()).toBe(false);

  const ctrl = mobileSmokePage.getByTestId("nav-ctrl");
  await ctrl.tap();
  await expect(ctrl).toHaveAttribute("aria-pressed", "true");
  const openPanelBox = await panel.boundingBox();
  expect(openPanelBox).not.toBeNull();
  await mobileSmokePage.mouse.click(8, Math.max(8, openPanelBox!.y / 2));
  await expect.poll(paneFocused).toBe(true);
  await mobileSmokePage.keyboard.type("c");
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(mobileSmokePage);
    return capture.batches.reduce((total, inputBatch) => total + inputBatch.data.length, 0);
  }).toBe(expectedDirectBytes.length + 1);
  const ctrlInputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(ctrlInputCapture.droppedBatches).toBe(0);
  expect(ctrlInputCapture.batches.every((inputBatch) => inputBatch.sessionId === sessionId)).toBe(true);
  expect(ctrlInputCapture.batches.flatMap((inputBatch) => inputBatch.data).at(-1)).toBe(0x03);
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");

  // Alt-screen occupancy is NOT a mouse request. This fixture app enters the alt
  // screen and never sets DECSET 1000/1002 — exactly like less, man and plain vim
  // — so with the toggle ON its wheel must stay NATIVE: reading intent, not one
  // byte to the PTY. Only once the app arms real tracking does the identical
  // gesture forward, and then it must recover the pending frame synchronously.
  const forwardAltNonce = `forward-${crypto.randomUUID().replaceAll("-", "").slice(0, 10)}`;
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    encodePtyFixtureCommand({
      op: "ALT_SCREEN",
      active: true,
      prefix: `FORWARD-ALT-${forwardAltNonce}-`,
      count: 8,
      nonce: forwardAltNonce,
    }),
  );
  await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: `ALT_READY:${forwardAltNonce}` });
  await expect(mouse).toHaveAttribute("aria-pressed", "true");
  const measureWheelTarget = () => terminalSlot.locator(".cell-grid").evaluate((element) => {
    if (!(element instanceof HTMLElement)) throw new Error("mobile terminal grid disappeared");
    const viewport = element.querySelector(".cell-viewport");
    const row = viewport?.querySelector(".cell-row");
    if (!(viewport instanceof HTMLElement) || !(row instanceof HTMLElement)) {
      throw new Error("mobile terminal cell geometry disappeared");
    }
    const gridRect = element.getBoundingClientRect();
    const cols = Number.parseInt(element.style.getPropertyValue("--cell-cols"), 10);
    const cellWidth = viewport.getBoundingClientRect().width / cols;
    const cellHeight = row.getBoundingClientRect().height;
    if (!Number.isFinite(cellWidth) || cellWidth <= 0 || cellHeight <= 0) {
      throw new Error("mobile terminal cell geometry is invalid");
    }
    // The detached key sheet covers the grid center. Aim the trusted wheel at
    // the unobscured top-left cell instead of accidentally scrolling the sheet.
    const x = gridRect.left + cellWidth / 2;
    const y = gridRect.top + cellHeight / 2;
    const hit = document.elementFromPoint(x, y);
    if (!hit || !element.contains(hit)) {
      throw new Error("mobile terminal forwarded-wheel target is occluded");
    }
    return {
      x,
      y,
      col: Math.max(1, 1 + Math.floor((x - gridRect.left) / cellWidth)),
      row: Math.max(1, 1 + Math.floor((y - gridRect.top) / cellHeight)),
    };
  });

  // NEGATIVE: mouse-indifferent app, toggle ON, trusted wheel — nothing forwards.
  const nativeWheelTarget = await measureWheelTarget();
  expect(nativeWheelTarget).toMatchObject({ col: 1, row: 1 });
  await resetTerminalInputCapture(mobileSmokePage);
  await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "wheel", {});
  await mobileSmokePage.mouse.move(nativeWheelTarget.x, nativeWheelTarget.y);
  await mobileSmokePage.mouse.wheel(0, 120);
  expect(await readImmediateTerminalPaintSample(mobileSmokePage)).toMatchObject({
    eventType: "wheel",
    trusted: true,
  });
  const nativeWheelCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(nativeWheelCapture.droppedBatches).toBe(0);
  expect(nativeWheelCapture.batches).toEqual([]);
  // Native reading, not a black hole: forwarding is admitted input and would have
  // kept the pane live, so the reading hold is the positive proof it never ran.
  await expect.poll(async () => {
    const presentation = (await readTerminalStreamProbe(mobileSmokePage, sessionId))
      .browser.presentation;
    return { intent: presentation?.reader_intent, reason: presentation?.reader_reason };
  }).toEqual({ intent: "reading", reason: "wheel" });

  // The app now asks for drag tracking + SGR-1006, the way a mouse-aware TUI does.
  // The mode rides the same frame as the marker the wheel hold keeps pending.
  const beforeForwardPending = await readTerminalStreamProbe(mobileSmokePage, sessionId);
  const forwardPendingMarker = `FORWARD-PENDING:${forwardAltNonce}`;
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    encodePtyFixtureCommand({
      op: "EMIT",
      text: `\x1b[?1002h\x1b[?1006h${forwardPendingMarker}`,
    }),
  );
  const forwardPending = await waitForCanonicalAdvance(
    mobileSmokePage,
    sessionId,
    beforeForwardPending,
  );
  expectCanonicalAdvanceHeld(beforeForwardPending, forwardPending, {
    readerReason: "wheel",
    selectionHold: false,
  });
  const hiddenForwardMarker = await attemptPaintedMarker(
    mobileSmokePage,
    sessionId,
    forwardPendingMarker,
  );
  expect(hiddenForwardMarker.proof).toBeNull();
  // POSITIVE: identical gesture on the identical cell, now that the app asked.
  await resetTerminalInputCapture(mobileSmokePage);
  await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "wheel", {
    marker: forwardPendingMarker,
  });
  const forwardWheelTarget = await measureWheelTarget();
  expect(forwardWheelTarget).toMatchObject({ col: 1, row: 1 });
  await mobileSmokePage.mouse.move(forwardWheelTarget.x, forwardWheelTarget.y);
  await mobileSmokePage.mouse.wheel(0, 120);
  const immediateForwardPaint = await readImmediateTerminalPaintSample(mobileSmokePage);
  expect(immediateForwardPaint).toMatchObject({
    eventType: "wheel",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateForwardPaint?.markerRowRect).not.toBeNull();
  const forwardInputCapture = await readTerminalInputCapture(mobileSmokePage);
  expect(forwardInputCapture.droppedBatches).toBe(0);
  expect(forwardInputCapture.batches).toEqual([{
    sessionId,
    data: Array.from(new TextEncoder().encode("\x1b[<65;1;1M")),
  }]);
  const forwardRecoveredProof = await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: forwardPendingMarker });
  expect(forwardRecoveredProof).toMatchObject({ marker: forwardPendingMarker, frames: 2 });
  expectRecoveredLive(forwardPending, await readTerminalStreamProbe(mobileSmokePage, sessionId));
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "false");
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "ALT_SCREEN", active: false, nonce: forwardAltNonce }),
  );
  await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: `ALT_EXIT:${forwardAltNonce}` });

  const portrait = mobileSmokePage.viewportSize();
  expect(portrait).not.toBeNull();
  await mobileSmokePage.setViewportSize({ width: portrait!.height, height: portrait!.width });
  const landscape = await expectOpenGeometry("landscape");
  expect(landscape.panelOverflowY).toBe("auto");
  expect(landscape.panelScrollHeight!).toBeGreaterThan(landscape.panelClientHeight!);
  await panel.evaluate((el) => { el.scrollTop = el.scrollHeight; });
  await expect.poll(() => panel.evaluate((el) => el.scrollTop)).toBeGreaterThan(0);
  await mobileSmokePage.setViewportSize(portrait!);
  await expectOpenGeometry("restored portrait");
  await expect.poll(async () => {
    const restored = await readGeometry();
    if (!restored?.panel) return Number.POSITIVE_INFINITY;
    return Math.max(
      rectDelta(restored.dock, closedGeometry!.dock),
      rectDelta(restored.terminal, closedGeometry!.terminal),
      rectDelta(restored.toggle, initialPortrait.toggle),
      rectDelta(restored.panel, initialPortrait.panel!),
    );
  }, { message: "portrait terminal and detached floating keypad geometry must recover" })
    .toBeLessThanOrEqual(2);

  await input.tap();
  await expect(input).toBeFocused();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);

  await mobileSmokePage.getByTestId("nav-ctrl").tap();
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "true");
  await toggle.tap();
  await expect(panel).toHaveCount(0);
  await expect(toggle).toHaveAttribute("data-open", "false");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);
  await toggle.tap();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("nav-ctrl")).toHaveAttribute("aria-pressed", "false");
  await expect(input).toHaveValue(draft);
  expect(await paneFocused()).toBe(false);
});

// A mic that cannot start used to leave the button red and data-state="listening"
// forever, with the reason rendered as an unreadable ~124x390 ribbon anchored to
// the 44px mic wrapper. Both halves are asserted here: the recording ENDS, and
// the caption spans the composer bar.
test("a mic that cannot start ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          const e = new Error("denied");
          e.name = "NotAllowedError";
          return Promise.reject(e);
        },
      },
    });
  });
  // Re-runs the module-scope transcription-config fetch with the key now set.
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);
  const toggle = mobileSmokePage.getByTestId("terminal-nav-toggle");
  const panel = mobileSmokePage.getByTestId("terminal-nav-buttons");
  await expect(toggle).toHaveAttribute("data-open", "false");
  await toggle.tap();
  await expect(panel).toBeVisible();

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();
  await expect(panel).toBeVisible();
  await expect(toggle).toHaveAttribute("data-open", "true");

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  const caption = mobileSmokePage.getByTestId("voice-caption");
  await expect(caption).toContainText("Mic blocked");
  const box = (await caption.boundingBox())!;
  expect(box.width).toBeGreaterThan(300);
  expect(box.x).toBeGreaterThanOrEqual(0);
});

// The reported bug that no error path covers: getUserMedia RESOLVES, the graph
// builds, and not one audio frame ever arrives (iOS suspends the AudioContext
// out from under the recording; AudioWorklet ships dead on some iPhone builds).
// That used to be a red mic forever with nothing recorded and nothing logged.
// The stub below is a complete Web Audio graph that never fires a callback, so
// BOTH the worklet and the ScriptProcessor repair path stay silent.
test("a mic that opens but delivers no audio ends the recording and says why", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    class DeadPort { onmessage: unknown = null; close() {} }
    class DeadWorkletNode { port = new DeadPort(); connect() {} disconnect() {} }
    class DeadContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = DeadContext;
    win.webkitAudioContext = DeadContext;
    win.AudioWorkletNode = DeadWorkletNode;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would
    // drop and claim the caption before the silence deadline. Only that ONE
    // host is faked (a permanently-open, send-swallowing socket); the SPA's own
    // Sync WS must keep using the real implementation.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null, onmessage: null, onerror: null, onclose: null,
        send() {}, close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();
  // The recording is genuinely live first — this is not a start failure.
  await expect(voice).toHaveAttribute("data-state", "listening");

  // Two silence windows (2.5s each): one rebuild attempt, then give up.
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mic).toHaveAttribute("data-recording", "false");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("sent no audio");
});

// Measured on the reporter's iPhone (iOS 18.7, installed PWA): the recording ran
// with ctx_state "suspended", frames 0, chunks 0, while the Deepgram socket was
// open — starting the mic switches the OS audio session and re-suspends a
// context created before the stream, and resume() does not bring it back. The
// stub reproduces exactly that: healthy until getUserMedia resolves, suspended
// afterwards, resume() a no-op. A pipeline that cannot render must fail the tap,
// not record silence.
test("an audio session suspended by the mic start fails the tap instead of recording silence", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    const win = window as unknown as Record<string, unknown>;
    let live: { state: string } | null = null;
    class IosContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      constructor() { live = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); } // iOS: outside a gesture this does nothing
      close() { return Promise.resolve(); }
    }
    class DeadPort { onmessage: unknown = null; close() {} }
    win.AudioContext = IosContext;
    win.webkitAudioContext = IosContext;
    win.AudioWorkletNode = class { port = new DeadPort(); connect() {} disconnect() {} };
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          if (live) live.state = "suspended"; // the OS audio-session switch
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  await mic.tap();

  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(mic).toHaveAttribute("data-recording", "false");
  // Names the real cause, and fails immediately instead of after two silence
  // windows — which is how you tell this apart from generic dead capture.
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("audio session stayed suspended");
});

// iOS can leave the activation-primer resume pending/rejected while
// getUserMedia switches the OS audio session. That first resume is not the
// liveness verdict: once the stream exists, the wired graph gets one
// authoritative resume and must send real PCM before Deepgram can transcribe.
test("mobile mic retries AudioContext resume after the input session opens", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let inputSessionOpen = false;
    let liveContext: { state: string } | null = null;
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;

    class RetryContext {
      state = "suspended";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      resumeCalls = 0;
      constructor() { liveContext = this; }
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() {
        this.resumeCalls++;
        if (this.resumeCalls === 1) {
          return Promise.reject(new Error("the audio session did not respond in time — tap the mic again"));
        }
        if (!inputSessionOpen) return Promise.reject(new Error("the input session is not open"));
        this.state = "running";
        return Promise.resolve();
      }
      close() {
        this.state = "closed";
        return Promise.resolve();
      }
    }
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }

    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = RetryContext;
    win.webkitAudioContext = RetryContext;
    win.AudioWorkletNode = LiveWorkletNode;
    const liveFrame = new Float32Array(2048).fill(0.25);
    setInterval(() => {
      if (inputSessionOpen && liveContext?.state === "running") {
        liveNode?.port.onmessage?.({ data: liveFrame });
      }
    }, 20);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          inputSessionOpen = true;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });

    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      let sentPcm = false;
      let announcedPcm = false;
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload instanceof ArrayBuffer) {
            sentPcm ||= payload.byteLength > 0;
            if (sentPcm && !announcedPcm) {
              announcedPcm = true;
              setTimeout(() => {
                sock.onmessage?.({
                  data: JSON.stringify({
                    type: "Results", is_final: false,
                    channel: { alternatives: [{ transcript: "PCM ready" }] },
                  }),
                });
              }, 0);
            }
            return;
          }
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: {
                  alternatives: [{
                    transcript: sentPcm ? "post-stream resume delivered audio" : "",
                  }],
                },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });

  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("PCM ready");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("post-stream resume delivered audio");
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);
});

// The report this whole class comes from: on a phone the FIRST recording works
// and every one after it is dead — the mic never leaves `listening`, or the stop
// wedges in `finalizing`, or the caption reads "Deepgram connection dropped".
// The graph here is genuinely LIVE (frames really flow) and the faked Deepgram
// socket really answers a Finalize, so nothing but the engine's own state can
// end a recording. The 4.5 s gap is longer than the mobile idle release
// (micIdle.releaseMs = 4 s on touch), so tap #2 pays a COLD device open exactly
// like the phone does instead of reusing a warm pipeline.
test("a second recording works exactly like the first", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }
    class LiveContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = LiveContext;
    win.webkitAudioContext = LiveContext;
    win.AudioWorkletNode = LiveWorkletNode;
    let emitInterim = false;
    win.__emitVoiceInterim = () => { emitInterim = true; };
    // 48 kHz × 40 ms = 1920 samples; 2048 clears the resampler's emit threshold,
    // so every tick delivers a real PCM chunk to the attached sink.
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: { getUserMedia: () => Promise.resolve({ getTracks: () => [{ stop() {} }] }) },
    });
    // The hermetic stack has no internet, so the real Deepgram socket would drop
    // and claim the caption. Only that ONE host is faked; the SPA's own Sync WS
    // must keep using the real implementation. This one ANSWERS a Finalize, so a
    // stop delivers a transcript instead of timing out into an empty send.
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: { alternatives: [{ transcript: "hello from the mic" }] },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => {
        sock.readyState = 1;
        sock.onopen?.();
        if (emitInterim) {
          sock.onmessage?.({
            data: JSON.stringify({
              type: "Results", is_final: false,
              channel: { alternatives: [{ transcript: "still recording" }] },
            }),
          });
        }
      }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");

  const send = mobileSmokePage.getByTestId("chat-send");
  await expect(input).not.toBeFocused();
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  await send.click();
  await expect(input).toHaveValue("");
  await expect(input).not.toBeFocused();
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  // Longer than the 4 s mobile idle release: recording two opens the device
  // cold, exactly like the reporter's second attempt.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic");
  await expect(input).not.toBeFocused();

  // Preserve the unsent-recordings contract: another finalized recording
  // appends instead of replacing the still-unsent second transcript.
  await mobileSmokePage.waitForTimeout(4_500);
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(input).toHaveValue("hello from the mic hello from the mic");
  await expect(input).not.toBeFocused();
  await expect(mobileSmokePage.getByTestId("voice-caption")).toHaveCount(0);

  // Hiding the only reachable composer must cancel the recording and restore
  // the pre-dictation draft, not retain an interim hypothesis as ordinary text.
  const settledDraft = "hello from the mic hello from the mic";
  await mobileSmokePage.evaluate(() => {
    const emitInterim = (window as unknown as { __emitVoiceInterim: () => void }).__emitVoiceInterim;
    emitInterim();
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue(`${settledDraft} still recording`);
  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(settledDraft);
  await expect(mobileSmokePage.getByTestId("mobile-voice-input")).toHaveAttribute("data-state", "idle");
});

test("Web Speech second recording starts from an empty sent draft", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {}
      stop() {
        const result = [{ transcript: "hello from web speech" }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  const mic = mobileSmokePage.getByTestId("voice-mic");
  const input = mobileSmokePage.getByTestId("chat-input");
  await expect(voice).toHaveAttribute("data-engine", "web-speech");

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");

  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(input).toHaveValue("");
  await expect.poll(() => readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });

  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await expect(input).toHaveValue("");
  expect(await readStoredComposerDraft(mobileSmokePage, sessionId)).toEqual({
    present: false,
    value: null,
  });
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle");
  await expect(input).toHaveValue("hello from web speech");
});

test("desktop passive drafting and dictation preserve a selected reader until pane park", async ({
  smokePage,
  stack,
  browserName,
}) => {
  const interimSpeech = "still speaking\npassive dictated line\nanother dictated line";
  const finalSpeech = "finished speech\nfinal dictated line\nlast finalized line";
  const interimDraft = `typed base ${interimSpeech}`;
  const committedDraft = `typed base ${finalSpeech}`;
  await stack.client.transcriptionSetConfig({ deepgramKey: "", deepgramLanguage: "en" });
  await smokePage.addInitScript(({ interimSpeech, finalSpeech }) => {
    interface FakeResult extends Array<{ transcript: string }> {
      isFinal: boolean;
    }
    interface FakeResultEvent {
      resultIndex: number;
      results: FakeResult[];
    }
    let speechStarts = 0;
    class FakeSpeechRecognition {
      continuous = false;
      interimResults = false;
      lang = "";
      onresult: ((event: FakeResultEvent) => void) | null = null;
      onend: (() => void) | null = null;
      onerror: ((event: { error: string }) => void) | null = null;
      start() {
        speechStarts += 1;
        const result = [{ transcript: interimSpeech }] as FakeResult;
        result.isFinal = false;
        queueMicrotask(() => this.onresult?.({ resultIndex: 0, results: [result] }));
      }
      stop() {
        const result = [{ transcript: finalSpeech }] as FakeResult;
        result.isFinal = true;
        this.onresult?.({ resultIndex: 0, results: [result] });
        queueMicrotask(() => this.onend?.());
      }
      abort() {
        queueMicrotask(() => this.onend?.());
      }
    }
    const speechWindow = window as unknown as Window & {
      SpeechRecognition: typeof FakeSpeechRecognition;
      webkitSpeechRecognition: typeof FakeSpeechRecognition;
      __speechStarts: () => number;
    };
    speechWindow.SpeechRecognition = FakeSpeechRecognition;
    speechWindow.webkitSpeechRecognition = FakeSpeechRecognition;
    speechWindow.__speechStarts = () => speechStarts;
  }, { interimSpeech, finalSpeech });
  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(
    () => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object",
  );

  const firstId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, firstId);
  const firstSlot = smokePage.getByTestId(`terminal-slot-${firstId}`);
  const firstDock = firstSlot.getByTestId("mobile-chat-input");
  const firstVoice = firstDock.getByTestId("mobile-voice-input");
  const firstInput = firstDock.getByTestId("chat-input");
  await expect(firstVoice).toHaveAttribute("data-engine", "web-speech");
  await firstDock.getByTestId("chat-box").evaluate(async (element) => {
    await Promise.all(element.getAnimations().map((animation) => animation.finished));
  });

  const readerSuffix = crypto.randomUUID().replaceAll("-", "").slice(0, 10);
  const readerPrefix = `DICTATION-READER-${readerSuffix}-`;
  await inputSmokeTerminal(
    smokePage,
    firstId,
    `for i in $(seq 1 120); do printf '${readerPrefix}%03d\\n' "$i"; done\r`,
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 20_000);
  }, { id: firstId, marker: `${readerPrefix}120` });
  const readerGridBox = await firstSlot.locator(".cell-grid").boundingBox();
  if (!readerGridBox) throw new Error("dictation reader grid geometry disappeared");
  await smokePage.mouse.move(
    readerGridBox.x + readerGridBox.width / 2,
    readerGridBox.y + readerGridBox.height / 2,
  );
  await smokePage.mouse.wheel(0, -1200);
  await expect.poll(async () =>
    (await readTerminalStreamProbe(smokePage, firstId)).browser.presentation?.at_bottom
  ).toBe(false);
  expect(await holdNativeTerminalSelection(smokePage, firstId)).toBe(true);
  const beforePassiveOutput = await readTerminalStreamProbe(smokePage, firstId);
  const passiveMarker = `DICTATION-PENDING-${readerSuffix}`;
  await inputSmokeTerminal(smokePage, firstId, `printf '%s\\n' ${passiveMarker}\r`);
  const passivePending = await waitForCanonicalAdvance(smokePage, firstId, beforePassiveOutput);
  expectCanonicalAdvanceHeld(beforePassiveOutput, passivePending, {
    readerReason: "selection",
    selectionHold: true,
  });
  expect((await attemptPaintedMarker(smokePage, firstId, passiveMarker)).proof).toBeNull();

  const readReaderGeometry = (rememberRow = false) => smokePage.evaluate(({ id, remember }) => {
    const probeWindow = window as Window & { __roostPassiveReaderRow?: HTMLElement };
    const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminal = slot?.querySelector(".wterm");
    const selection = window.getSelection();
    const range = selection?.rangeCount ? selection.getRangeAt(0) : null;
    const endpoint = selection?.anchorNode instanceof Element
      ? selection.anchorNode
      : selection?.anchorNode?.parentElement;
    const row = endpoint?.closest(".cell-row");
    if (
      !(terminal instanceof HTMLElement)
      || !(row instanceof HTMLElement)
      || !range
      || !selection
      || selection.isCollapsed
    ) return null;
    if (remember) probeWindow.__roostPassiveReaderRow = row;
    const rowRect = row.getBoundingClientRect();
    const rangeRect = range.getBoundingClientRect();
    return {
      rangeConnected:
        row.isConnected
        && range.startContainer.isConnected
        && range.endContainer.isConnected,
      rangeOnRow:
        row.contains(range.startContainer)
        && row.contains(range.endContainer),
      sameRow: probeWindow.__roostPassiveReaderRow === row,
      scrollTop: terminal.scrollTop,
      selected: selection.toString(),
      rowText: row.textContent ?? "",
      rowRect: {
        left: rowRect.left, top: rowRect.top, right: rowRect.right, bottom: rowRect.bottom,
      },
      rangeRect: {
        left: rangeRect.left, top: rangeRect.top, right: rangeRect.right, bottom: rangeRect.bottom,
      },
    };
  }, { id: firstId, remember: rememberRow });
  const readerBaseline = await readReaderGeometry(true);
  if (!readerBaseline) throw new Error("selected reader geometry was unavailable");
  expect(readerBaseline.rangeConnected).toBe(true);
  expect(readerBaseline.rangeOnRow).toBe(true);
  expect(readerBaseline.sameRow).toBe(true);
  expect(readerBaseline.rowText).toContain(readerPrefix);
  expect(readerBaseline.rangeRect.bottom).toBeGreaterThan(readerBaseline.rangeRect.top);
  const expectReaderPreserved = async (label: string) => {
    const current = await readReaderGeometry();
    expect(current, `${label}: selected reader geometry`).not.toBeNull();
    expect(current!.rangeConnected, `${label}: connected native range`).toBe(true);
    expect(current!.rangeOnRow, `${label}: native range endpoints`).toBe(true);
    expect(current!.sameRow, `${label}: selected row identity`).toBe(true);
    expect(current!.selected, `${label}: selected text`).toBe(readerBaseline.selected);
    expect(current!.rowText, `${label}: selected history row`).toBe(readerBaseline.rowText);
    expect(Math.abs(current!.scrollTop - readerBaseline.scrollTop), `${label}: scrollTop`).toBeLessThanOrEqual(1);
    for (const surface of ["rowRect", "rangeRect"] as const) {
      for (const edge of ["left", "top", "right", "bottom"] as const) {
        expect(
          Math.abs(current![surface][edge] - readerBaseline[surface][edge]),
          `${label}: ${surface}.${edge}`,
        ).toBeLessThanOrEqual(1);
      }
    }
    const presentation = (await readTerminalStreamProbe(smokePage, firstId)).browser.presentation;
    expect(presentation).toMatchObject({
      reader_intent: "reading",
      reader_reason: "selection",
      hold_mask: { selection: true, link: false },
      reconciled: passivePending.browser.dom_reconciled,
    });
  };

  await firstInput.click();
  await expectReaderPreserved("composer focus");
  await firstInput.press("x");
  await expect(firstInput).toHaveValue("x");
  await expectReaderPreserved("composer trusted key edit");
  await firstInput.press("y");
  await expect(firstInput).toHaveValue("xy");
  await expectReaderPreserved("composer repeated trusted key edit");
  await firstInput.fill("typed base");
  await expectReaderPreserved("composer edit");
  const singleLineComposerHeight = await firstInput.evaluate((element) =>
    element.getBoundingClientRect().height);
  await firstInput.fill("typed base\npassive autogrow line\nanother passive line");
  await expect.poll(() => firstInput.evaluate((element) =>
    element.getBoundingClientRect().height)).toBeGreaterThan(singleLineComposerHeight + 1);
  await expectReaderPreserved("composer autogrow");
  await firstInput.fill("typed base");
  await expectReaderPreserved("composer autogrow shrink");
  await firstInput.press("Space");
  if (browserName === "chromium") {
    const imeSession = await smokePage.context().newCDPSession(smokePage);
    try {
      await imeSession.send("Input.imeSetComposition", {
        text: "中",
        selectionStart: 1,
        selectionEnd: 1,
      });
      await imeSession.send("Input.insertText", { text: "中" });
    } finally {
      await imeSession.detach();
    }
  } else {
    // Non-Chromium engines have no CDP IME endpoint. Keep their branch native
    // and trusted rather than synthesizing DOM events; Chromium above owns the
    // required composition contract regardless of the Playwright project name.
    await smokePage.keyboard.insertText("中");
  }
  await expect(firstInput).toHaveValue("typed base 中");
  await expectReaderPreserved("composer IME");
  await firstInput.fill("typed base");
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(interimDraft);
  await expect.poll(() => firstInput.evaluate((element) =>
    element.getBoundingClientRect().height)).toBeGreaterThan(singleLineComposerHeight + 1);
  await expectReaderPreserved("dictation interim");

  await firstInput.press("Enter");
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(interimDraft);
  await expectReaderPreserved("dictation Enter");

  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "idle");
  await expect(firstInput).toHaveValue(committedDraft);
  await expectReaderPreserved("dictation final");

  await firstDock.getByTestId("chat-send").click();
  await expect(firstInput).toHaveValue("");
  await expect.poll(readReaderGeometry).toBeNull();
  await firstInput.fill("typed base");

  const secondId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(smokePage, secondId);
  const secondDock = smokePage
    .getByTestId(`terminal-slot-${secondId}`)
    .getByTestId("mobile-chat-input");
  await expect(secondDock).toBeVisible();
  await expect(secondDock.getByTestId("voice-mic")).toBeVisible();
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  await switchToSmokeSession(smokePage, firstId);
  await expect(firstDock).toBeVisible();
  await expect(firstInput).toHaveValue("typed base");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");

  // Spotlight keeps covered pane DOM mounted, so it must explicitly deactivate
  // that pane's recorder and expose the spotlit pane's mic.
  await pressPlatformShortcut(smokePage, "splitRight", "D");
  await expect.poll(() => smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.filter((slot) => {
      const rect = slot.getBoundingClientRect();
      return rect.right > 0 && rect.left < innerWidth && rect.bottom > 0 && rect.top < innerHeight;
    }).length)).toBe(2);
  const visibleIds = await smokePage.locator("[data-pane-slot]").evaluateAll((slots) =>
    slots.flatMap((slot) => {
      const rect = slot.getBoundingClientRect();
      const testId = slot.getAttribute("data-testid") ?? "";
      return rect.right > 0 && rect.left < innerWidth && testId.startsWith("terminal-slot-")
        ? [testId.slice("terminal-slot-".length)]
        : [];
    }));
  const spotlightId = visibleIds.find((id) => id !== firstId);
  expect(spotlightId).toBeTruthy();
  const spotlightSlot = smokePage.getByTestId(`terminal-slot-${spotlightId!}`);
  const spotlightDock = spotlightSlot.getByTestId("mobile-chat-input");

  // Both controls can already be activation targets when two clicks arrive in
  // one task. The first claim must atomically exclude the retained second target.
  const startsBeforeRace = await smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts());
  await smokePage.evaluate(([firstSessionId, secondSessionId]) => {
    const firstMic = document.querySelector(
      `[data-testid="terminal-slot-${firstSessionId}"] [data-testid="voice-mic"]`,
    );
    const secondMic = document.querySelector(
      `[data-testid="terminal-slot-${secondSessionId}"] [data-testid="voice-mic"]`,
    );
    if (!(firstMic instanceof HTMLElement) || !(secondMic instanceof HTMLElement)) {
      throw new Error("both split-pane microphones must exist before the activation race");
    }
    firstMic.click();
    secondMic.click();
  }, [firstId, spotlightId!] as const);
  await expect.poll(() => smokePage.evaluate(() =>
    (window as unknown as Window & { __speechStarts: () => number }).__speechStarts()))
    .toBe(startsBeforeRace + 1);
  await expect(smokePage.locator(
    '[data-testid="mobile-voice-input"][data-state="listening"]',
  )).toHaveCount(1);
  await expect(smokePage.getByTestId("voice-mic")).toHaveCount(1);
  await smokePage.getByTestId("voice-discard").click();
  await expect(firstDock.getByTestId("voice-mic")).toBeVisible();
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();

  await firstInput.click();
  await firstDock.getByTestId("voice-mic").click();
  await expect(firstVoice).toHaveAttribute("data-state", "listening");
  await expect(firstInput).toHaveValue(`typed base ${interimSpeech}`);
  await expect(spotlightDock.getByTestId("voice-mic")).toHaveCount(0);
  await spotlightSlot.getByTestId("terminal-display").click();
  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(spotlightSlot).toHaveAttribute("data-spotlit", "true");
  await expect(firstDock).toHaveAttribute("data-active", "false");
  await expect(firstDock).toHaveAttribute("aria-hidden", "true");
  await expect(firstDock).toHaveAttribute("inert", "");
  await expect(spotlightDock).toHaveAttribute("data-active", "true");
  await expect(spotlightDock).not.toHaveAttribute("aria-hidden", "true");
  await expect(firstVoice).toHaveAttribute("data-state", "idle");
  await expect(firstInput).toHaveValue("typed base");
  await expect(spotlightDock.getByTestId("voice-mic")).toBeVisible();
  // Compact suppresses the spotlight surface but retains its stored ID. The
  // visible viewport composer must become active rather than a silent mic shell.
  await smokePage.setViewportSize({ width: 390, height: 844 });
  const compactDock = smokePage.getByTestId("mobile-chat-input");
  await expect(compactDock).toHaveCount(1);
  await expect(compactDock).toHaveAttribute("data-placement", "viewport");
  await compactDock.getByTestId("voice-mic").click();
  await expect(compactDock.getByTestId("mobile-voice-input")).toHaveAttribute(
    "data-state",
    "listening",
  );
  await compactDock.getByTestId("voice-discard").click();
  await smokePage.keyboard.press("Escape");
});

// Chromium's ordinary media-device behavior cannot reproduce WebKit's
// never-settling promise (and passes on the unfixed code), so
// the stall is injected: getUserMedia parks forever on the FIRST call only.
// Unfixed, that tap never resolves — `warming` keeps the dead promise for the
// page's lifetime, so the mic breathes in `listening` and EVERY later tap awaits
// the same corpse. Fixed, the tap ends with a reason and the next one opens a
// fresh device.
test("a stalled mic open ends the recording and the next tap still works", async ({ mobileSmokePage, stack }) => {
  await stack.client.transcriptionSetConfig({ deepgramKey: "smoke-test-key", deepgramLanguage: "en" });
  await mobileSmokePage.addInitScript(() => {
    let liveNode: { port: { onmessage: ((e: { data: Float32Array }) => void) | null } } | null = null;
    class LiveWorkletNode {
      port = { onmessage: null as ((e: { data: Float32Array }) => void) | null, close() {} };
      constructor() { liveNode = this; }
      connect() {}
      disconnect() {}
    }
    class LiveContext {
      state = "running";
      sampleRate = 48000;
      destination = {};
      audioWorklet = { addModule: () => Promise.resolve() };
      createMediaStreamSource() { return { connect() {}, disconnect() {} }; }
      createScriptProcessor() { return { onaudioprocess: null, connect() {}, disconnect() {} }; }
      resume() { return Promise.resolve(); }
      close() { return Promise.resolve(); }
    }
    const win = window as unknown as Record<string, unknown>;
    win.AudioContext = LiveContext;
    win.webkitAudioContext = LiveContext;
    win.AudioWorkletNode = LiveWorkletNode;
    setInterval(() => liveNode?.port.onmessage?.({ data: new Float32Array(2048).fill(0.3) }), 40);
    let opens = 0;
    Object.defineProperty(navigator, "mediaDevices", {
      configurable: true,
      value: {
        getUserMedia: () => {
          opens++;
          // iOS 18.7 as an installed PWA: the OS audio session is mid-transition
          // and the promise simply never settles. Nothing cancels it.
          if (opens === 1) return Promise.withResolvers<MediaStream>().promise;
          return Promise.resolve({ getTracks: () => [{ stop() {} }] });
        },
      },
    });
    const RealWebSocket = window.WebSocket;
    const PatchedWebSocket = function (url: string, protocols?: string | string[]) {
      if (!String(url).includes("api.deepgram.com")) return new RealWebSocket(url, protocols);
      const sock = {
        url, readyState: 0,
        onopen: null as (() => void) | null,
        onmessage: null as ((e: { data: string }) => void) | null,
        onerror: null, onclose: null,
        send(payload: unknown) {
          if (payload !== '{"type":"Finalize"}') return;
          setTimeout(() => {
            sock.onmessage?.({
              data: JSON.stringify({
                type: "Results", is_final: true, from_finalize: true,
                channel: { alternatives: [{ transcript: "hello from the mic" }] },
              }),
            });
          }, 0);
        },
        close() { sock.readyState = 3; },
      };
      setTimeout(() => { sock.readyState = 1; sock.onopen?.(); }, 10);
      return sock;
    } as unknown as typeof WebSocket;
    Object.assign(PatchedWebSocket, { CONNECTING: 0, OPEN: 1, CLOSING: 2, CLOSED: 3 });
    win.WebSocket = PatchedWebSocket;
  });
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await mobileSmokePage.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  const sessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, sessionId);

  const voice = mobileSmokePage.getByTestId("mobile-voice-input");
  await expect(voice).toHaveAttribute("data-engine", "deepgram");
  const mic = mobileSmokePage.getByTestId("voice-mic");

  // The stalled tap must END, with the deadline's own caption.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 20_000 });
  await expect(mobileSmokePage.getByTestId("voice-caption")).toContainText("did not respond in time");

  // …and the singleton must be usable again on the very next tap.
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "listening");
  await mic.tap();
  await expect(voice).toHaveAttribute("data-state", "idle", { timeout: 15_000 });
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue(/hello from the mic/);
});

test("mobile composer keeps an unsent draft per session, across panes and reloads", async ({ mobileSmokePage, stack }) => {
  const firstId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(mobileSmokePage, firstId);
  await expectSmokeComposer(mobileSmokePage);

  const box = mobileSmokePage.getByTestId("chat-box");
  const input = mobileSmokePage.getByTestId("chat-input");
  await input.fill("half typed message");

  const drawer = mobileSmokePage.getByTestId("sidebar-drawer");
  await mobileSmokePage.getByTestId("mobile-deck-bar-menu").tap();
  await expect(drawer).toHaveAttribute("data-open", "true");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-buttons")).toHaveCount(0);

  await mobileSmokePage.getByTestId("brand-row-collapse").tap();
  await expect(drawer).toHaveAttribute("data-open", "false");
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Body-portaled controls must leave with the terminal surface. The persistent
  // deck stays mounted behind /file, so hiding only its host is insufficient.
  await mobileSmokePage.evaluate((workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { navigate(href: string): void };
    }).__smoke;
    smoke.navigate(`/file/${workerFp}/tmp/roost-composer-missing.txt`);
  }, stack.workerFp);
  await expect(mobileSmokePage.getByTestId("file-viewer-sheet")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("mobile-chat-input")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("terminal-nav-toggle")).toHaveCount(0);
  await mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { navigate(href: string): void };
    }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, firstId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Tapping the terminal blurs the field but keeps both bar and draft.
  await mobileSmokePage.getByTestId(`terminal-slot-${firstId}`).click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue("half typed message");

  // Escape has the same single-layer behavior: dismiss focus, retain content.
  await input.tap();
  await expect(input).toBeFocused();
  await input.press("Escape");
  await expect(box).toBeVisible();
  await expect(input).not.toBeFocused();
  await expect(input).toHaveValue("half typed message");

  // A different session gets its own empty draft; coming back restores this one.
  const secondId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
  await switchToSmokeSession(mobileSmokePage, firstId);
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // A reload restores it (localStorage, local device only).
  await mobileSmokePage.reload({ waitUntil: "domcontentloaded" });
  await expect(mobileSmokePage.getByTestId(`terminal-slot-${firstId}`)).toBeVisible();
  await expectSmokeComposer(mobileSmokePage);
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("half typed message");

  // Send is the only thing that consumes it, and does not dismiss the dock.
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  await expect(mobileSmokePage.getByTestId("chat-input")).toHaveValue("");
});

test("mobile composer preserves and submits terminal input", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iPhone terminal input contract");
  const sessionId = await mobileSmokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await mobileSmokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = mobileSmokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await expectSmokeComposer(mobileSmokePage);

  const box = mobileSmokePage.getByTestId("chat-box");
  const initialInput = mobileSmokePage.getByTestId("chat-input");
  await expect(initialInput).not.toBeFocused();
  await initialInput.tap();
  await expect(initialInput).toBeFocused();

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await initialInput.fill("  x  ");
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<  x  >");
  await expect(box).toBeVisible();
  await expect(initialInput).toHaveValue("");

  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect.poll(() => mobileSmokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { paneFocused(sessionId: string): { focused: boolean } };
    }).__smoke;
    return smoke.paneFocused(id).focused;
  }, sessionId)).toBe(true);
  const directMarker = `MOBILE_DIRECT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await mobileSmokePage.keyboard.type(`printf '%s\\n' ${directMarker}`);
  await mobileSmokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(directMarker);

  // The same permanent three-control bar accepts another draft.
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await initialInput.fill("y");
  await expect(mobileSmokePage.getByTestId("chat-lead")).toHaveCount(0);
  await expect(mobileSmokePage.getByTestId("chat-send")).toBeVisible();

  // Tapping outside and Escape each blur without discarding or unmounting.
  await slot.click({ position: { x: 8, y: 8 } });
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect(initialInput).toHaveValue("y");
  await initialInput.tap();
  await initialInput.press("Escape");
  await expect(box).toBeVisible();
  await expect(initialInput).not.toBeFocused();
  await expect(initialInput).toHaveValue("y");

  // Enter is a NEWLINE, never a submit: only the send button commits the draft.
  const composed = mobileSmokePage.getByTestId("chat-input");
  await composed.fill("printf 'ENTER_LINE_A\\n'");
  await composed.press("Enter");
  await mobileSmokePage.keyboard.type("printf 'ENTER_LINE_B\\n'");
  await expect(composed).toHaveValue("printf 'ENTER_LINE_A\\n'\nprintf 'ENTER_LINE_B\\n'");
  await expect(box).toBeVisible();
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_A");
  await expect.poll(() => slot.textContent()).toContain("ENTER_LINE_B");
  await expect(composed).toHaveValue("");
  await expect(box).toBeVisible();

  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    `IFS= read -r line; printf '<%s>\\n' "$line"\r`,
  );
  await mobileSmokePage.getByTestId("chat-send").click();
  await expect.poll(() => slot.textContent()).toContain("<>");
  await expect(composed).toHaveValue("");
  await expect(box).toBeVisible();

  const secondSessionId = (await spawnSmokeShell(mobileSmokePage, stack.workerFp)).session_id;
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expectSmokeComposer(mobileSmokePage);
  const secondInput = mobileSmokePage.getByTestId("chat-input");
  await expect(secondInput).not.toBeFocused();
  await secondInput.tap();
  await expect(secondInput).toBeFocused();
  await secondInput.press("Escape");
  await expect(mobileSmokePage.getByTestId("chat-box")).toBeVisible();
  await expect(secondInput).not.toBeFocused();
  await switchToSmokeSession(mobileSmokePage, sessionId);
  await expectSmokeComposer(mobileSmokePage);
  await switchToSmokeSession(mobileSmokePage, secondSessionId);
  await expectSmokeComposer(mobileSmokePage);
});

test("trusted keyboard input and bottom-follow behavior", async ({ smokePage, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  const marker = `PW_INPUT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await smokePage.keyboard.type(`printf '%s\\n' ${marker}`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain(marker);

  await smokePage.keyboard.type("for i in $(seq 1 500); do echo BOTTOMLINE-$i; done");
  await smokePage.keyboard.press("Enter");
  await expect.poll(async () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { renderProbe(sessionId: string): { rowCount: number; atBottom: boolean } } }).__smoke;
    return smoke.renderProbe(id);
  }, sessionId)).toMatchObject({ atBottom: true });
});

test("streaming sequence repair leaves an off-bottom reader fixed", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "for i in $(seq 1 1500); do printf 'READERLINE-%04d stable-history\\n' $i; done\r");
  }, sessionId);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { markerScan(sessionId: string, prefix: string): { max: number } };
    }).__smoke;
    return smoke.markerScan(id, "READERLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(1500);

  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("streaming reader grid missing");
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -6000);
  const sample = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        cellGridEpoch(sessionId: string): string;
        droppedCellFrameCount(sessionId: string): number;
      };
    }).__smoke;
    const container = document.querySelector(
      `[data-testid="terminal-slot-${id}"] .wterm.cell-grid`,
    ) as HTMLElement;
    const rect = container.getBoundingClientRect();
    const row = document.elementFromPoint(rect.left + 100, rect.top + 200)?.closest(".cell-row");
    return {
      scrollTop: container.scrollTop,
      scrollHeight: container.scrollHeight,
      row: row?.textContent ?? "",
      rowOffset: row ? row.getBoundingClientRect().top - (rect.top + 200) : null,
      frames: smoke.cellFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      fullFrames: smoke.cellFullFrameCount(id),
      dropped: smoke.droppedCellFrameCount(id),
    };
  }, sessionId);
  await expect.poll(async () => (await sample()).row, { timeout: 10_000 })
    .toContain("READERLINE-");
  const before = await sample();

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        dropNextCellFrame(sessionId: string): void;
        input(sessionId: string, text: string): Promise<void>;
      };
    }).__smoke;
    smoke.dropNextCellFrame(id);
    await smoke.input(
      id,
      "for i in $(seq 1501 1800); do printf 'READERLINE-%04d streaming\\n' $i; sleep 0.01; done\r",
    );
  }, sessionId);
  await expect.poll(() => sample(), { timeout: 60_000 }).toMatchObject({
    fullFrames: before.fullFrames + 1,
    dropped: before.dropped + 1,
  });
  await expect.poll(async () => (await sample()).frames, { timeout: 60_000 })
    .toBeGreaterThan(before.frames + 200);

  const during = await sample();
  expect(during.gridEpoch).toBe(before.gridEpoch);
  expect(during.scrollTop).toBe(before.scrollTop);
  expect(during.scrollHeight).toBe(before.scrollHeight);
  expect(during.row).toBe(before.row);
  expect(during.rowOffset).toBe(before.rowOffset);

  await smokePage.mouse.wheel(0, 100_000);
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number };
      };
    }).__smoke;
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: smoke.markerScan(id, "READERLINE-").max,
    };
  }, sessionId), { timeout: 30_000 }).toEqual({ atBottom: true, markerMax: 1800 });
});

test("alternate screen survives width and height perturbations", async ({ smokePage, stack }) => {
  test.setTimeout(240_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await expect(smokePage.getByTestId(`terminal-slot-${sessionId}`)).toBeVisible();
  await smokePage.keyboard.type(`'${fixturePath}'`);
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => smokePage.getByTestId(`terminal-slot-${sessionId}`).textContent()).toContain("CELLLINE-60");
  const result = await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        runRenderStress(options: {
          sessionId: string; prefix: string; screen: "main" | "alt"; iterations: number;
        }): Promise<{ verdict: string; fails: unknown[] }>;
      };
    }).__smoke;
    return smoke.runRenderStress({ sessionId: id, prefix: "CELLLINE-", screen: "alt", iterations: 80 });
  }, sessionId);
  expect(result).toMatchObject({ verdict: "PASS", fails: [] });
  await smokePage.keyboard.press("q");
});

test("two viewers preserve ordered terminal markers", async ({ smokePage, browser, stack }) => {
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  await smokePage.keyboard.type("for i in $(seq 1 120); do echo MULTIVIEW-$i; done");
  await smokePage.keyboard.press("Enter");
  const passiveContext = await browser.newContext({ viewport: { width: 1400, height: 900 } });
  await passiveContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
  const passive = await passiveContext.newPage();
  try {
    await passive.goto(`${stack.baseUrl}/s/${sessionId}`);
    await passive.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(true));
    await smokePage.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await smokePage.setViewportSize({ width: 700 + (iteration % 2) * 50, height: 500 + (iteration % 3) * 40 });
    }
    const scan = await passive.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(scan).toMatchObject({ duplicated: [], outOfOrder: 0 });
    await passive.bringToFront();
    for (let iteration = 0; iteration < 24; iteration++) {
      await passive.setViewportSize({ width: 1350 + (iteration % 2) * 50, height: 820 + (iteration % 3) * 40 });
    }
    await smokePage.bringToFront();
    const primaryScan = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { markerScan(sessionId: string, prefix: string): unknown } }).__smoke;
      return smoke.markerScan(id, "MULTIVIEW-");
    }, sessionId);
    expect(primaryScan).toMatchObject({ duplicated: [], outOfOrder: 0 });
  } finally {
    await smokePage.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passive.evaluate(() => (window as unknown as Window & { __smoke: { forceVisible(on: boolean): void } }).__smoke.forceVisible(false)).catch(() => undefined);
    await passiveContext.close();
  }
});

// A fresh deep-session attach starts with the current viewport and a truthful
// spacer only. History stays off the network and out of the DOM until demand.
test("deep-history attach/reveal paints the live tail until history is requested", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(120_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() =>
    typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);

  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): {
          mode: "cell" | "byte" | "none";
          rowCount: number;
          atBottom: boolean;
        };
        markerScan(sessionId: string, prefix: string): {
          max: number;
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      ...smoke.renderProbe(id),
      markerMax: smoke.markerScan(id, "CELLLINE-").max,
      snapshotSbRows: smoke.lastFullFrameSbRows(id),
      historyRequests: smoke.scrollbackBackfillRequestCount(id),
    };
  }, sessionId);

  const attached = await probe();
  expect(attached).toMatchObject({
    mode: "cell",
    markerMax: 8000,
    atBottom: true,
    snapshotSbRows: 0,
    historyRequests: 0,
  });
  expect(attached.rowCount).toBeLessThan(100);

  const idleSamples = await smokePage.evaluate(async ({ id, rowCount, requests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{ rowCount: number; requests: number; atBottom: boolean }> = [];
    for (let frame = 0; frame < 8; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      samples.push({
        rowCount: smoke.renderProbe(id).rowCount,
        requests: smoke.scrollbackBackfillRequestCount(id),
        atBottom: smoke.renderProbe(id).atBottom,
      });
    }
    return { samples, expected: { rowCount, requests, atBottom: true } };
  }, { id: sessionId, rowCount: attached.rowCount, requests: attached.historyRequests });
  expect(idleSamples.samples).toEqual(Array(8).fill(idleSamples.expected));

  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm") as HTMLElement | null;
    if (!container) return;
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
    }).__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: attached.historyRequests });
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { rowCount: number } };
    }).__smoke;
    return smoke.renderProbe(id).rowCount > previous;
  }, { id: sessionId, previous: attached.rowCount });

  const demanded = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    return smoke.markerScan(id, "CELLLINE-");
  }, sessionId);
  expect(demanded).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
});

// OSC 8 hyperlinks are CORE-AUTHORED per cell — never derived from the byte
// stream, never matched by TEXT. This is the scenario the old text→URI matcher
// could not express: two links with IDENTICAL visible text and DIFFERENT URIs on
// one row. A text→URI map holds one entry per text, so the second link either
// overwrote the first or was dropped, and BOTH anchors then pointed at the same
// place (and the same text appearing anywhere else became a link too). Per-cell
// identity keeps them apart. The run then has to survive a scrollback backfill
// round-trip, which serves history rows from the live core's own scrollback —
// a second, independent path to the same per-cell link data.
test("identical link text with different URIs keeps both, through a backfill round-trip", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop cell paint contract");
  test.setTimeout(120_000);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, fixtureWorker);
  await navigateToSmokeSession(smokePage, sessionId);
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: PTY_FIXTURE_READY });

  // Every VISIBLE token is kept short on purpose: the whole emit is ONE row, and
  // a row that reaches the pane's grid width soft-wraps — which would split
  // `linkMarker` across two rows, and findPaintedMarker matches per row. 55
  // columns fits any plausible desktop pane.
  // Leading 'z' also on purpose: a pure-hex suffix is a valid bare commit SHA,
  // which the INFERRED GitHub-ref pass would linkify on its own and muddy the
  // count (the leading word character defeats that regex's lookbehind).
  const suffix = `z${crypto.randomUUID().replaceAll("-", "").slice(0, 7)}`;
  const linkText = `r-${suffix}`;
  const firstUri = `https://osc8.test/${suffix}/one`;
  const secondUri = `https://osc8.test/${suffix}/two`;
  const hyperlink = (uri: string): string => `\u001b]8;;${uri}\u0007${linkText}\u001b]8;;\u0007`;
  const linkMarker = `O8-${suffix}`;
  // A plain URL and a resolvable file path on the SAME row. Producer links are
  // painted by the renderer while these two are found by the inferred scan, and
  // the authorities must coexist: the scan's "already handled" mark is a MARK,
  // not "this row has an anchor", or a painted link would suppress every regex
  // and file link sharing its row.
  const inferredUrl = `https://i.test/${suffix}`;
  const filePath = "s/f.ts:9";
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({
      op: "EMIT",
      text: `${hyperlink(firstUri)} ${hyperlink(secondUri)} ${inferredUrl} ${filePath} ${linkMarker}`,
    }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: linkMarker });

  const paintedLinks = (): Promise<Array<{ href: string | null; key: string | null }>> =>
    smokePage.evaluate(({ id, text }) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      return Array.from(slot?.querySelectorAll("a.wterm-link[data-link-key]") ?? [])
        .filter((anchor) => (anchor.textContent ?? "") === text)
        .map((anchor) => ({
          href: anchor.getAttribute("href"),
          key: anchor.getAttribute("data-link-key"),
        }));
    }, { id: sessionId, text: linkText });
  const inferredUrls = (): Promise<string[]> =>
    smokePage.evaluate((id) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const selector = 'a.wterm-link:not([data-link-key]):not([data-kind="file"])';
      return Array.from(slot?.querySelectorAll(selector) ?? [])
        .map((anchor) => anchor.getAttribute("href") ?? "");
    }, sessionId);
  const fileLinks = (): Promise<Array<{ href: string; text: string }>> =>
    smokePage.evaluate((id) => {
      const slot = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      return Array.from(slot?.querySelectorAll('a.wterm-link[data-kind="file"]') ?? [])
        .map((anchor) => ({
          href: anchor.getAttribute("href") ?? "",
          text: anchor.textContent ?? "",
        }));
    }, sessionId);

  const live = await paintedLinks();
  expect(live.map((link) => link.href)).toEqual([firstUri, secondUri]);
  // Distinct run identity is what keeps two identically-styled, identically-
  // texted links from coalescing into one span and losing a URI.
  expect(new Set(live.map((link) => link.key)).size).toBe(2);
  // The inferred scan is asynchronous (idle/rAF), unlike the painted anchors,
  // which exist the instant the row paints.
  await expect.poll(inferredUrls, { timeout: 15_000, intervals: [100] }).toEqual([inferredUrl]);
  const liveFiles = await fileLinks();
  expect(liveFiles.map((link) => link.text)).toEqual([filePath]);
  // The internal file route, not a browser navigation — Roost opens it itself.
  expect(liveFiles[0].href.startsWith("/file/")).toBe(true);

  // Push the link row deep into retained history, then reload: the authoritative
  // snapshot paints only the live tail plus a spacer, so the anchors leave the
  // DOM entirely and can only come back through a history page.
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "FLOOD", prefix: `OSC8-FILL-${suffix}-`, count: 200 }),
  );
  await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 30_000);
  }, { id: sessionId, marker: `OSC8-FILL-${suffix}-200` });

  await smokePage.reload({ waitUntil: "domcontentloaded" });
  await smokePage.waitForFunction(() =>
    typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
  await smokePage.waitForFunction((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    return !!pane?.querySelector(".cell-sb-spacer") && !!pane.querySelector(".cell-row");
  }, sessionId);
  expect(await paintedLinks()).toEqual([]);
  expect(await inferredUrls()).toEqual([]);
  expect(await fileLinks()).toEqual([]);

  const beforeHistory = await smokePage.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.scrollbackBackfillRequestCount(id);
  }, sessionId);
  await smokePage.evaluate((id) => {
    const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const container = pane?.querySelector(".wterm");
    if (!(container instanceof HTMLElement)) throw new Error("terminal has no scroll container");
    container.scrollTop = 0;
    container.dispatchEvent(new Event("scroll"));
  }, sessionId);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: beforeHistory });

  await expect.poll(
    async () => (await paintedLinks()).map((link) => link.href),
    { timeout: 30_000, intervals: [100] },
  ).toEqual([firstUri, secondUri]);
  expect(new Set((await paintedLinks()).map((link) => link.key)).size).toBe(2);
  await expect.poll(inferredUrls, { timeout: 15_000, intervals: [100] }).toEqual([inferredUrl]);
  expect((await fileLinks()).map((link) => link.text)).toEqual([filePath]);
});

// Returning to an already-open pane keeps the Sync socket but reclaims an
// authoritative snapshot. Hidden and offscreen panes must receive no cells.
test("returning to a dormant pane reclaims once without a Sync re-dial", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck/visibility contract");
  const spawn = (folder: string) => smokePage.evaluate(async ({ workerFp, dir }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, dir)).session_id;
  }, { workerFp: stack.workerFp, dir: folder });

  const sessionA = await spawn("/tmp");
  const probe = () => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        renderProbe(sessionId: string): { atBottom: boolean };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
        cellFrameCount(sessionId: string): number;
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "CELLLINE-");
    return {
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionA);

  await smokePage.goto(`${stack.baseUrl}/s/${sessionA}`);
  const slotA = smokePage.getByTestId(`terminal-slot-${sessionA}`);
  await expect(slotA).toBeVisible();
  await smokePage.keyboard.type("seq -f 'CELLLINE-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slotA.textContent(), { timeout: 60_000 }).toContain("CELLLINE-8000");
  await smokePage.waitForTimeout(1000);

  // Deck switch: A withdraws while offscreen, then one claim snapshot restores
  // the still-mounted renderer. The shared Sync socket stays open.
  const sessionB = await spawn("/tmp");
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { state(): { sessions: Record<string, unknown> } } }).__smoke;
    return id in smoke.state().sessions;
  }, sessionB)).toBe(true);
  const beforeSwitch = await probe();
  expect(beforeSwitch.markerMax).toBe(8000);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionB);
  await expect(smokePage.getByTestId(`tab-${sessionB}`)).toHaveAttribute("data-active", "true");
  await smokePage.waitForTimeout(1000);
  expect((await probe()).frames).toBe(beforeSwitch.frames);

  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionA);
  await expect(smokePage.getByTestId(`tab-${sessionA}`)).toHaveAttribute("data-active", "true");
  await expect.poll(async () => (await probe()).fullFrames).toBe(beforeSwitch.fullFrames + 1);
  expect(await probe()).toMatchObject({
    atBottom: true,
    markerMax: 8000,
    duplicated: [],
    outOfOrder: 0,
    wsGeneration: beforeSwitch.wsGeneration,
  });

  // Deterministic hidden pin: the page stays schedulable while lifecycle
  // handlers withdraw A. Output advances at the PTY but no cell reaches the
  // browser until visibility returns and one authoritative snapshot reclaims it.
  const beforeHide = await probe();
  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(true);
  });
  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & { __smoke: { input(sessionId: string, text: string): Promise<void> } }).__smoke;
    await smoke.input(id, "for i in $(seq 8001 8200); do echo CELLLINE-$i; sleep 0.01; done\r");
  }, sessionA);
  await smokePage.waitForTimeout(3000);
  expect(await probe()).toMatchObject({
    markerMax: 8000,
    frames: beforeHide.frames,
    fullFrames: beforeHide.fullFrames,
    wsGeneration: beforeHide.wsGeneration,
  });

  await smokePage.evaluate(() => {
    const smoke = (window as unknown as Window & { __smoke: { forceHidden(on: boolean): void } }).__smoke;
    smoke.forceHidden(false);
  });
  await expect.poll(async () => (await probe()).markerMax, { timeout: 60_000 }).toBe(8200);
  const afterShow = await probe();
  expect(afterShow).toMatchObject({
    atBottom: true,
    markerMax: 8200,
    duplicated: [],
    outOfOrder: 0,
    fullFrames: beforeHide.fullFrames + 1,
    wsGeneration: beforeHide.wsGeneration,
  });
});

test("offline producer divergence reconnects and repaints without a reload", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop transport recovery contract");
  test.setTimeout(90_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    sessionId,
  )).toBeGreaterThan(0);
  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-READY-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-READY-001");
  await waitForStableCellFrames(smokePage, sessionId);

  const canary = `offline-${sessionId}`;
  await setRecoveryCanary(smokePage, canary);
  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      fullFrames: smoke.cellFullFrameCount(id),
      gridEpoch: smoke.cellGridEpoch(id),
      wsGeneration: smoke.syncWsGeneration(),
    };
  }, sessionId);

  const context = smokePage.context();
  await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.pauseSyncTransport(),
  );
  try {
    await context.setOffline(true);
    await expect.poll(() => smokePage.evaluate(() => navigator.onLine)).toBe(false);
    await stack.client.sessionsInput({
      sessionId,
      data: new TextEncoder().encode(
        "for i in $(seq 1 30); do printf 'OFFLINE-RECOVER-%03d\\n' \"$i\"; sleep 0.01; done; seq 1 48; printf 'OFFLINE-CURRENT-%03d\\n' 1\r",
      ),
    });
    await expect.poll(async () => {
      const cells = await stack.client.sessionsGetScrollbackCells({
        sessionId,
        endRow: BigInt(Number.MAX_SAFE_INTEGER),
        maxRows: 250,
        gridEpoch: before.gridEpoch,
      });
      const text = cells.rows
        .map((row) => row.spans.map((span) => span.text || " ").join(""))
        .join("\n");
      return Math.max(0, ...Array.from(text.matchAll(/OFFLINE-RECOVER-(\d+)/g), (match) => Number(match[1])));
    }, { timeout: 30_000, intervals: [100] }).toBe(30);
    const isolated = await smokePage.evaluate((id) => {
      const smoke = (window as unknown as { __smoke: RecoverySmokeApi }).__smoke;
      return {
        frames: smoke.cellFrameCount(id),
        max: smoke.markerScan(id, "OFFLINE-RECOVER-").max,
      };
    }, sessionId);
    expect(isolated).toEqual({ frames: before.frames, max: 0 });
  } finally {
    await context.setOffline(false);
    await smokePage.evaluate(
      () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.resumeSyncTransport(),
    );
  }

  await expect.poll(() => smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  ), { timeout: 30_000, intervals: [100] }).toBeGreaterThan(before.wsGeneration);
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  ), { timeout: 30_000, intervals: [100] }).toContain("OFFLINE-CURRENT-001");

  const recovered = await recoveryProbe(smokePage, sessionId, "OFFLINE-RECOVER-");
  expect(recovered).toMatchObject({
    canary,
    atBottom: true,
    scan: { max: 0, duplicated: [], missing: 0, outOfOrder: 0 },
  });
  expect(await smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.lastFullFrameSbRows(id),
    sessionId,
  )).toBe(0);
  const afterReconnect = await smokePage.evaluate((id) => ({
    fullFrames: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.cellFullFrameCount(id),
    wsGeneration: (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  }), sessionId);
  expect(afterReconnect.fullFrames).toBe(before.fullFrames + 1);

  await smokePage.evaluate(
    async (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.input(
      id,
      "printf 'OFFLINE-AFTER-%03d\\n' 1\r",
    ),
    sessionId,
  );
  await expect.poll(() => smokePage.evaluate(
    (id) => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.viewportText(id),
    sessionId,
  )).toContain("OFFLINE-AFTER-001");
  expect(await smokePage.evaluate(
    () => (window as unknown as { __smoke: RecoverySmokeApi }).__smoke.syncWsGeneration(),
  )).toBe(afterReconnect.wsGeneration);
});

test("long hidden deep-history resume paints the current viewport before history", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop visibility and geometry contract");
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

// A stale pane may retain deep painted history while parked, but its catch-up
// frame replaces that obsolete image with only the current viewport. No
// history page may race the reveal or move the reader away from the bottom.
test("deck switch to a stale deep-history pane lands at the live bottom instantly", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  test.setTimeout(180_000);
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & {
      __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
    }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SWL-%g' 1 8000");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 60_000 }).toContain("SWL-8000");

  const siblings: string[] = [];
  for (let index = 0; index < 5; index++) {
    const siblingId = await smokePage.evaluate(async (workerFp) => {
      const smoke = (window as unknown as Window & {
        __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> };
      }).__smoke;
      return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
    }, stack.workerFp);
    await smokePage.waitForFunction((id) => {
      const smoke = (window as unknown as Window & {
        __smoke: { state(): { sessions: Record<string, unknown> } };
      }).__smoke;
      return id in smoke.state().sessions;
    }, siblingId);
    await smokePage.evaluate((id) => {
      const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
      smoke.navigate(`/s/${id}`);
    }, siblingId);
    await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
    siblings.push(siblingId);
  }
  await smokePage.waitForTimeout(1200);

  const before = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFrameCount(sessionId: string): number;
        renderProbe(sessionId: string): { rowCount: number; atBottom: boolean };
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    return {
      frames: smoke.cellFrameCount(id),
      requests: smoke.scrollbackBackfillRequestCount(id),
      ...smoke.renderProbe(id),
    };
  }, sessionId);
  expect(before.rowCount).toBeGreaterThan(1500);
  expect(before.atBottom).toBe(true);

  await smokePage.evaluate(async (id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { input(sessionId: string, text: string): Promise<void> };
    }).__smoke;
    await smoke.input(id, "seq -f 'FRESH-%g' 1 300\r");
  }, sessionId);
  await smokePage.waitForTimeout(500);
  expect(await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFrameCount(sessionId: string): number };
    }).__smoke;
    return smoke.cellFrameCount(id);
  }, sessionId)).toBe(before.frames);

  await smokePage.getByTestId(`tab-${sessionId}`).click();
  const reveal = await smokePage.evaluate(async ({ id, priorRequests }) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        lastFullFrameSbRows(sessionId: string): number;
        scrollbackBackfillRequestCount(sessionId: string): number;
      };
    }).__smoke;
    const samples: Array<{
      visibleFresh: number;
      painted: number;
      top: number;
      height: number;
      client: number;
      snapshotSbRows: number;
      historyRequests: number;
    }> = [];
    let authoritativeAt = -1;
    for (let frame = 0; frame < 180; frame++) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      const pane = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
      const container = pane?.querySelector(".wterm") as HTMLElement | null;
      if (!container) continue;
      const box = container.getBoundingClientRect();
      let visibleFresh = -1;
      for (const row of container.querySelectorAll(".cell-row")) {
        const rowBox = row.getBoundingClientRect();
        if (rowBox.bottom <= box.top + 1 || rowBox.top >= box.bottom - 1) continue;
        const match = (row.textContent ?? "").match(/FRESH-(\d+)/);
        if (match) visibleFresh = Math.max(visibleFresh, Number(match[1]));
      }
      samples.push({
        visibleFresh,
        painted: container.querySelectorAll(".cell-row").length,
        top: container.scrollTop,
        height: container.scrollHeight,
        client: container.clientHeight,
        snapshotSbRows: smoke.lastFullFrameSbRows(id),
        historyRequests: smoke.scrollbackBackfillRequestCount(id),
      });
      if (visibleFresh === 300 && authoritativeAt < 0) authoritativeAt = samples.length - 1;
      if (authoritativeAt >= 0 && samples.length >= authoritativeAt + 8) break;
    }
    return { samples, authoritativeAt, priorRequests };
  }, { id: sessionId, priorRequests: before.requests });

  expect(reveal.authoritativeAt).toBeGreaterThanOrEqual(0);
  const authoritative = reveal.samples[reveal.authoritativeAt]!;
  expect(authoritative.top).toBeGreaterThanOrEqual(authoritative.height - authoritative.client - 2);
  expect(authoritative.snapshotSbRows).toBe(0);
  expect(authoritative.historyRequests).toBe(reveal.priorRequests);
  expect(authoritative.painted).toBeLessThan(100);
  expect(reveal.samples.slice(reveal.authoritativeAt).map((sample) => ({
    painted: sample.painted,
    historyRequests: sample.historyRequests,
  }))).toEqual(Array(reveal.samples.length - reveal.authoritativeAt).fill({
    painted: authoritative.painted,
    historyRequests: reveal.priorRequests,
  }));

  // Reader demand is a trusted gesture: a programmatic scrollTop write keeps the
  // renderer's bottom pin, so only a real wheel expresses "show me history".
  const revealedBox = await smokePage.getByTestId(`terminal-slot-${sessionId}`).boundingBox();
  if (!revealedBox) throw new Error("revealed pane has no box to scroll");
  await smokePage.mouse.move(
    revealedBox.x + revealedBox.width / 2,
    revealedBox.y + revealedBox.height / 2,
  );
  await smokePage.mouse.wheel(0, -6000);
  await smokePage.waitForFunction(({ id, previous }) => {
    const smoke = (window as unknown as Window & {
      __smoke: { scrollbackBackfillRequestCount(sessionId: string): number };
    }).__smoke;
    return smoke.scrollbackBackfillRequestCount(id) > previous;
  }, { id: sessionId, previous: reveal.priorRequests });
  await smokePage.waitForFunction((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    const scan = smoke.markerScan(id, "SWL-");
    return scan.duplicated.length === 0 && scan.missing === 0 && scan.outOfOrder === 0;
  }, sessionId);
  const scan = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        markerScan(sessionId: string, prefix: string): {
          duplicated: number[];
          missing: number;
          outOfOrder: number;
        };
      };
    }).__smoke;
    return smoke.markerScan(id, "SWL-");
  }, sessionId);
  expect(scan).toMatchObject({ duplicated: [], missing: 0, outOfOrder: 0 });
});

// Bottom-follow must survive geometry changes that happen while a pane is
// parked: the box shrinks (window resize / keyboard inset / divider drag),
// nothing re-samples the bottom, and pre-noteBoxResize the pane revealed
// off-bottom with live output landing below the fold — permanently.
test("a pane revealed after the window shrank is still at the bottom", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop scroll-geometry contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'SHRK-%g' 1 600");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("SHRK-600");

  // Park A behind a sibling, then shrink the window UNDER the parked pane.
  const siblingId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, siblingId);
  await expect(smokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute("data-active", "true");
  const vp = smokePage.viewportSize()!;
  // −100 px keeps the short side ≥ 600 (windowSizeClass compact boundary keys
  // on min(w,h)): the pane must SHRINK, not flip the whole app to mobile UI.
  await smokePage.setViewportSize({ width: vp.width, height: vp.height - 100 });
  await smokePage.waitForTimeout(400); // park restyle + ResizeObserver tick

  // Reveal: the FIRST sample with painted rows is already at the bottom.
  await smokePage.evaluate((id) => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    const samples: Array<{ painted: number; top: number; height: number; client: number }> = [];
    w.__shrkSamples = samples;
    w.__shrkTimer = window.setInterval(() => {
      if (!c) return;
      samples.push({
        painted: c.querySelectorAll(".cell-row").length,
        top: c.scrollTop, height: c.scrollHeight, client: c.clientHeight,
      });
    }, 50);
  }, sessionId);
  await smokePage.getByTestId(`tab-${sessionId}`).click();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { renderProbe(sessionId: string): { atBottom: boolean; rowCount: number } };
    }).__smoke;
    const p = smoke.renderProbe(id);
    return p.rowCount > 0 && p.atBottom;
  }, sessionId), { timeout: 1_500, intervals: [50] }).toBe(true);
  await smokePage.waitForTimeout(1_000);
  const samples = await smokePage.evaluate(() => {
    const w = window as unknown as Window & { __shrkSamples?: Array<{ painted: number; top: number; height: number; client: number }>; __shrkTimer?: number };
    if (w.__shrkTimer !== undefined) window.clearInterval(w.__shrkTimer);
    return w.__shrkSamples ?? [];
  });
  expect(samples.length).toBeGreaterThan(5);
  const offBottom = samples.filter((s) => s.painted > 0 && s.top < s.height - s.client - 2);
  expect(offBottom).toEqual([]);
  // The newest marker is visible at the bottom of the SHRUNKEN box.
  const lastVisible = await smokePage.evaluate((id) => {
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const c = slotEl?.querySelector(".wterm") as HTMLElement | null;
    if (!c) return -1;
    const box = c.getBoundingClientRect();
    let max = -1;
    for (const row of c.querySelectorAll(".cell-row")) {
      const r = row.getBoundingClientRect();
      if (r.bottom <= box.top + 1 || r.top >= box.bottom - 1) continue;
      const m = (row.textContent ?? "").match(/SHRK-(\d+)/);
      if (m) max = Math.max(max, Number(m[1]));
    }
    return max;
  }, sessionId);
  expect(lastVisible).toBe(600);
});

// A /file/… visit must NOT tear down the terminal deck: MainPane's screens
// share ONE route definition (App.tsx) and the deck host only flips
// visibility (MainPane.tsx). Separate <Route> entries remounted MainPane on
// every /s ↔ /file crossing — every renderer died, warmSessionIds reset, and
// the return was a cold mount + claim storm. Node identity + a flat full-frame
// count are the remount detectors; nothing else in this suite navigates /file,
// which is exactly why the regression slipped past green runs.
test("a /file round-trip keeps the deck warm and costs no snapshot", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop deck-persistence contract");
  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: { spawnShell(worker: string, folder: string): Promise<{ session_id: string }> } }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await smokePage.keyboard.type("seq -f 'FRT-%g' 1 300");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent(), { timeout: 30_000 }).toContain("FRT-300");

  const baseline = await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: { cellFullFrameCount(sessionId: string): number; syncWsGeneration(): number };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (deck) deck.__persistCanary = "alive";
    return { fullFrames: smoke.cellFullFrameCount(id), wsGeneration: smoke.syncWsGeneration(), canarySet: !!deck };
  }, sessionId);
  expect(baseline.canarySet).toBe(true);

  // Into the file viewer: deck stays in the DOM, merely visibility-hidden.
  await smokePage.evaluate((fp) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/file/${fp}/tmp/roost-frt-missing.txt`);
  }, stack.workerFp);
  await expect.poll(() => smokePage.evaluate(() => {
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    return { canary: deck.__persistCanary ?? null, hostVis: getComputedStyle(deck.parentElement).visibility };
  })).toEqual({ canary: "alive", hostVis: "hidden" });

  // The retained folder keeps viewport state warm, not keyboard ownership.
  // A reserved deck chord while the file viewer owns the surface must not
  // mutate the hidden layout.
  await pressPlatformShortcut(smokePage, "spotlight", "Enter");
  await expect(slot).not.toHaveAttribute("data-spotlit", "true");

  // And back: the SAME deck node, zero full frames, zero re-dials — the
  // return is a pure visibility flip, and the pane is still at the bottom.
  await smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & { __smoke: { navigate(href: string): void } }).__smoke;
    smoke.navigate(`/s/${id}`);
  }, sessionId);
  await expect(slot).toBeVisible();
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as Window & {
      __smoke: {
        cellFullFrameCount(sessionId: string): number;
        syncWsGeneration(): number;
        renderProbe(sessionId: string): { atBottom: boolean; rowCount: number };
        markerScan(sessionId: string, prefix: string): { max: number; duplicated: number[]; outOfOrder: number };
      };
    }).__smoke;
    const deck = document.querySelector('[data-testid="terminal-deck"]') as (HTMLElement & { __persistCanary?: string }) | null;
    if (!deck?.parentElement) return null;
    const scan = smoke.markerScan(id, "FRT-");
    return {
      canary: deck.__persistCanary ?? null,
      hostVis: getComputedStyle(deck.parentElement).visibility,
      fullFrames: smoke.cellFullFrameCount(id),
      wsGeneration: smoke.syncWsGeneration(),
      atBottom: smoke.renderProbe(id).atBottom,
      markerMax: scan.max,
      duplicated: scan.duplicated,
      outOfOrder: scan.outOfOrder,
    };
  }, sessionId)).toEqual({
    canary: "alive",
    hostVis: "visible",
    fullFrames: baseline.fullFrames,
    wsGeneration: baseline.wsGeneration,
    atBottom: true,
    markerMax: 300,
    duplicated: [],
    outOfOrder: 0,
  });
});
