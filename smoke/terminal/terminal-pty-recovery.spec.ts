import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  shortcutPlatform,
  pressPlatformShortcut,
  spawnPtyFixtureSession,
  spawnSmokeShell,
  navigateToSmokeSession,
  switchToSmokeSession,
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
} from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  waitForCanonicalAdvance,
  expectCanonicalAdvanceHeld,
  expectRecoveredLive,
  activeCoordSubscriptionCount,
  coordViewerCount,
  workerViewerClaimCount,
} from "./terminal-probe-helpers.ts";

// @serial: the heaviest fixture case in the suite — a PTY-fixture worker plus
// dozens of painted-marker and cursor-geometry proofs on 10s budgets. Those are
// timeliness assertions, so it is measured alone rather than given looser
// budgets that would stop it detecting a real stall.
test("real PTY input recovers held rendering and rejected same-generation reclaim self-heals @serial", async ({
  smokePage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop terminal recovery and reclaim reproduction");
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
