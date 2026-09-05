import type { Locator, Page } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  pressPlatformShortcut,
  pasteFromClipboard,
  inputSmokeTerminal,
  resetTerminalInputCapture,
  readTerminalInputCapture,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import {
  holdNativeTerminalSelection,
  attemptPaintedMarker,
  attemptPaintedCursor,
  armImmediateTerminalPaintSample,
  readImmediateTerminalPaintSample,
  readTerminalLayoutGeometry,
} from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  waitForSettledTerminalRowsBelow,
  waitForCanonicalAdvance,
  expectCanonicalAdvanceHeld,
  expectRecoveredLive,
} from "./terminal-probe-helpers.ts";

type CursorAndImeRecoveryOptions = {
  page: Page;
  grid: Locator;
  sessionId: string;
  suffix: string;
  fixtureArmMs: number;
};

export async function proveCursorAndImeRecovery({
  page,
  grid,
  sessionId,
  suffix,
  fixtureArmMs,
}: CursorAndImeRecoveryOptions): Promise<void> {
  // A smoke-backdoor byte is deliberately passive. The fixture answers it with
  // CSI 1 C (cursor-forward/right), advancing the canonical cursor while the
  // selected reader remains on the old geometry. The next admitted trusted key
  // must adopt that exact pending frame inside keydown, preserve every clean row
  // node, clear only this pane's Selection, and pin.
  const cursorNonce = `cursor-${suffix}`;
  const cursorReady = `ARMED:CURSOR_MOVE:${cursorNonce}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_CURSOR_MOVE", nonce: cursorNonce }),
  );
  await page.evaluate(({ id, marker, timeoutMs }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, timeoutMs);
  }, { id: sessionId, marker: cursorReady, timeoutMs: fixtureArmMs });
  await waitForStableCellFrames(page, sessionId);
  const baselineCursor = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedCursor(id, undefined, 10_000);
  }, sessionId);
  const cursor = grid.locator(".cell-cursor");
  await expect(cursor).toHaveAttribute("data-blink", "true");
  await expect.poll(() => cursor.evaluate((element) => getComputedStyle(element).animationName))
    .not.toBe("none");
  const hiddenFrameCount = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: "\x1b[?25l", newline: false }),
  );
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId)).toBeGreaterThan(hiddenFrameCount);
  await expect(cursor).toHaveAttribute("data-blink", "true");
  await expect(cursor).toHaveAttribute("data-visible", "false");
  await expect(cursor).toHaveCSS("display", "none");
  expect((await attemptPaintedCursor(page, sessionId, {})).proof).toBeNull();
  const shownFrameCount = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId);
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: "\x1b[?25h", newline: false }),
  );
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.cellFrameCount(id);
  }, sessionId)).toBeGreaterThan(shownFrameCount);
  const shownCursor = await page.evaluate(({ id, row, column }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedCursor(id, { row, column }, 10_000);
  }, { id: sessionId, row: baselineCursor.row, column: baselineCursor.column });
  expect(shownCursor).toMatchObject({
    row: baselineCursor.row,
    column: baselineCursor.column,
    frames: 2,
  });
  expect(shownCursor.terminalClip).toEqual(baselineCursor.terminalClip);
  expect(shownCursor.visualViewport).toEqual(baselineCursor.visualViewport);
  await expect(cursor).toHaveAttribute("data-visible", "true");
  await expect(cursor).toHaveCSS("display", "block");
  const geometryBeforeIndicator = await readTerminalLayoutGeometry(page, sessionId);
  const beforeCursor = await readTerminalStreamProbe(page, sessionId);
  expect(beforeCursor.browser.handler_canonical).toEqual(beforeCursor.browser.dom_reconciled);
  expect(await holdNativeTerminalSelection(page, sessionId)).toBe(true);
  await inputSmokeTerminal(page, sessionId, "x");
  const cursorPending = await waitForCanonicalAdvance(page, sessionId, beforeCursor);
  expectCanonicalAdvanceHeld(beforeCursor, cursorPending, {
    readerReason: "selection",
    selectionHold: true,
  });
  const indicator = page.getByTestId("terminal-stream-indicator");
  await expect(indicator).toHaveAttribute("data-state", "catching_up");
  await expect(indicator).toHaveAttribute("title", "Screen catching up");
  const geometryWithIndicator = await readTerminalLayoutGeometry(page, sessionId, true);
  expect(geometryWithIndicator).toMatchObject({
    slot: geometryBeforeIndicator.slot,
    terminal: geometryBeforeIndicator.terminal,
    clientWidth: geometryBeforeIndicator.clientWidth,
    clientHeight: geometryBeforeIndicator.clientHeight,
    position: "absolute",
    pointerEvents: "none",
  });
  await expect(indicator).toHaveAttribute("aria-hidden", "true");
  expect(cursorPending.browser.replica.resync_latched).toBe(false);
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
  const staleCursorGeometry = await attemptPaintedCursor(page, sessionId, {
    row: baselineCursor.row,
    column: baselineCursor.column,
  });
  if (!staleCursorGeometry.proof) {
    throw new Error(`baseline cursor lost clipped geometry: ${staleCursorGeometry.error}`);
  }
  expect(staleCursorGeometry.proof.rect).toEqual(baselineCursor.rect);
  expect(staleCursorGeometry.proof.terminalClip).toEqual(baselineCursor.terminalClip);
  expect(staleCursorGeometry.proof.visualViewport).toEqual(baselineCursor.visualViewport);
  const hiddenCanonicalCursor = await attemptPaintedCursor(page, sessionId, {
    row: canonicalCursor.row,
    column: canonicalCursor.column,
  });
  expect(hiddenCanonicalCursor.proof).toBeNull();
  expect(hiddenCanonicalCursor.error).toContain("cursor presentation geometry was not proven");

  // Escape is admitted PTY data without adding a cursor prediction or browser
  // scrolling default; CSI 1 C remains the sole cursor transition.
  await resetTerminalInputCapture(page);
  await armImmediateTerminalPaintSample(page, sessionId, "keydown", {
    cursorRow: canonicalCursor.row,
  });
  await page.keyboard.press("Escape");
  const immediateKeyPaint = await readImmediateTerminalPaintSample(page);
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
    const capture = await readTerminalInputCapture(page);
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
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: "\x1b[?25h", newline: false }),
  );
  await expect(indicator).toHaveAttribute("data-state", "receiving");
  await expect(indicator).toHaveAttribute("title", "Receiving terminal frames");
  const afterTrustedKey = await readTerminalStreamProbe(page, sessionId);
  expect(afterTrustedKey.browser.presentation?.cursor.canonical).toEqual(canonicalCursor);
  expect(afterTrustedKey.browser.presentation?.cursor.dom).toMatchObject({
    ...canonicalCursor,
    connected: true,
  });
  expect(afterTrustedKey.browser.replica.resync_latched).toBe(false);
  expectRecoveredLive(cursorPending, afterTrustedKey);

  const recoveredCursor = await page.evaluate(({ id, row, column }) => {
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
  await expect(indicator).toHaveCount(0, { timeout: 2_000 });

  const liveAfterKey = `LIVE-AFTER-KEY:${suffix}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: liveAfterKey }),
  );
  const liveAfterKeyProof = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: liveAfterKey });
  expect(liveAfterKeyProof).toMatchObject({ marker: liveAfterKey, frames: 2 });
  expect((await readTerminalStreamProbe(page, sessionId)).browser.presentation?.reader_intent)
    .toBe("live");

  // Native wheel is explicit reading. Its next frame remains canonical-only;
  // committed IME input resumes live in the input event that admits its bytes.

  const gridBox = await grid.boundingBox();
  if (!gridBox) throw new Error("terminal grid geometry disappeared");
  await page.mouse.move(gridBox.x + gridBox.width / 2, gridBox.y + gridBox.height / 2);
  await page.mouse.wheel(0, -1200);
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    return {
      intent: probe.browser.presentation?.reader_intent,
      reason: probe.browser.presentation?.reader_reason,
      atBottom: probe.browser.presentation?.at_bottom,
    };
  }).toEqual({ intent: "reading", reason: "wheel", atBottom: false });
  const beforeWheelPending = await readTerminalStreamProbe(page, sessionId);
  const wheelPendingMarker = `WHEEL-PENDING:${suffix}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: wheelPendingMarker }),
  );
  const wheelPending = await waitForCanonicalAdvance(page, sessionId, beforeWheelPending);
  expectCanonicalAdvanceHeld(beforeWheelPending, wheelPending, {
    readerReason: "wheel",
    selectionHold: false,
  });
  const hiddenWheelMarker = await attemptPaintedMarker(page, sessionId, wheelPendingMarker);
  expect(hiddenWheelMarker.proof).toBeNull();
  expect(hiddenWheelMarker.error).toContain("not visibly painted");
  await armImmediateTerminalPaintSample(page, sessionId, "input", {
    marker: wheelPendingMarker,
  });
  await page.keyboard.insertText("中");
  const immediateImePaint = await readImmediateTerminalPaintSample(page);
  expect(immediateImePaint).toMatchObject({
    eventType: "input",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateImePaint?.markerRowRect).not.toBeNull();
  const imeRecoveredProof = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: wheelPendingMarker });
  expect(imeRecoveredProof).toMatchObject({ marker: wheelPendingMarker, frames: 2 });
  expectRecoveredLive(
    wheelPending,
    await readTerminalStreamProbe(page, sessionId),
    { predictiveCursor: true },
  );
  await waitForStableCellFrames(page, sessionId);
}

type FindPasteRecoveryOptions = {
  page: Page;
  sessionId: string;
  suffix: string;
  historyPrefix: string;
};

export async function proveFindPasteRecovery({
  page,
  sessionId,
  suffix,
  historyPrefix,
}: FindPasteRecoveryOptions): Promise<void> {
  const preFindRows = (await readTerminalStreamProbe(page, sessionId))
    .browser.presentation?.rows.dom;
  if (preFindRows === undefined) throw new Error("pre-find presentation omitted DOM rows");
  // A real one-line paste, not closing Find, adopts its pending frame.
  await pressPlatformShortcut(page, "terminalFind", "F");
  const findInput = page.getByTestId("terminal-find-input");
  await expect(findInput).toBeVisible();
  const findMarker = `${historyPrefix}20`;
  await findInput.fill(findMarker);
  await expect(page.getByTestId("terminal-find-count")).toHaveText("1/1", { timeout: 10_000 });
  const findProof = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: findMarker });
  expect(findProof).toMatchObject({ marker: findMarker, frames: 2 });
  await expect.poll(async () => {
    const presentation = (await readTerminalStreamProbe(page, sessionId)).browser.presentation;
    return { intent: presentation?.reader_intent, reason: presentation?.reader_reason };
  }).toEqual({ intent: "reading", reason: "find" });
  const findOpenRows = await waitForSettledTerminalRowsBelow(page, sessionId, preFindRows);
  await findInput.press("Escape");
  await expect(findInput).toHaveCount(0);
  await expect.poll(async () => {
    const browser = (await readTerminalStreamProbe(page, sessionId)).browser;
    const presentation = browser.presentation;
    const domRows = presentation?.rows.dom ?? null;
    const canonicalRows = presentation?.rows.canonical ?? null;
    const effectiveRows = browser.view.status === "accepted" && browser.view.active
      ? browser.view.effective_rows
      : null;
    return {
      resizedBehindReader: canonicalRows !== null
        && canonicalRows > findOpenRows
        && effectiveRows === canonicalRows
        && browser.replica.baseline_ready,
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

  const beforeFindPending = await readTerminalStreamProbe(page, sessionId);
  const findPendingMarker = `FIND-PENDING:${suffix}`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: findPendingMarker }),
  );
  const findPending = await waitForCanonicalAdvance(page, sessionId, beforeFindPending);
  expectCanonicalAdvanceHeld(beforeFindPending, findPending, {
    readerReason: "find",
    selectionHold: false,
  });
  const hiddenFindMarker = await attemptPaintedMarker(page, sessionId, findPendingMarker);
  expect(hiddenFindMarker.proof).toBeNull();
  const origin = new URL(page.url()).origin;
  await page.context().grantPermissions(["clipboard-read", "clipboard-write"], { origin });
  const pasteText = `pasted-${suffix}`;
  await page.evaluate((text) => navigator.clipboard.writeText(text), pasteText);
  await resetTerminalInputCapture(page);
  await armImmediateTerminalPaintSample(page, sessionId, "paste", {
    marker: findPendingMarker,
  });
  await pasteFromClipboard(page);
  const immediatePastePaint = await readImmediateTerminalPaintSample(page);
  expect(immediatePastePaint).toMatchObject({
    eventType: "paste",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediatePastePaint?.markerRowRect).not.toBeNull();
  const pasteRecoveredProof = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: findPendingMarker });
  expect(pasteRecoveredProof).toMatchObject({ marker: findPendingMarker, frames: 2 });
  await expect.poll(async () =>
    (await readTerminalInputCapture(page)).batches.flatMap((batch) => batch.data)
  ).toEqual(Array.from(new TextEncoder().encode(pasteText)));
  expectRecoveredLive(
    findPending,
    await readTerminalStreamProbe(page, sessionId),
    { predictiveCursor: true },
  );
}
