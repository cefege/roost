import type { Locator, Page } from "@playwright/test";
import type { TerminalStreamProbe } from "../../apps/web/src/lib/smoke.ts";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  inputSmokeTerminal,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";
import {
  holdNativeTerminalSelection,
  attemptPaintedMarker,
  armImmediateTerminalPaintSample,
  readImmediateTerminalPaintSample,
} from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  waitForCanonicalAdvance,
  expectCanonicalAdvanceHeld,
  expectRecoveredLive,
  coordinatorTerminalViewState,
} from "./terminal-probe-helpers.ts";

type ComposerRecoveryOptions = {
  page: Page;
  slot: Locator;
  sessionId: string;
  suffix: string;
  fixtureArmMs: number;
  fixtureWorkerFp: string;
};

type ComposerRecoveryResult = {
  presented: TerminalStreamProbe;
  overwriteMarker: string;
};

export async function proveComposerRecovery({
  page,
  slot,
  sessionId,
  suffix,
  fixtureArmMs,
  fixtureWorkerFp,
}: ComposerRecoveryOptions): Promise<ComposerRecoveryResult> {
  // Grow the desktop composer and let its debounced terminal-view resize settle
  // before creating the changed-epoch frame. The resulting new-stream baseline
  // is independent canonical state; the interaction contract below starts from
  // the newest stable geometry and proves Send adopts the later held alt-screen.
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
  await waitForStableCellFrames(page, sessionId);

  // A changed-epoch alternate-screen response is now pending before Send. The
  // admitted callback must paint it inside the click dispatch while the tall
  // composer is still at its pre-shrink geometry.
  const altNonce = `alt-${suffix}`;
  const altReady = `ARMED:ALT_REDRAW:${altNonce}:line`;
  const altMarker = `ALT_REDRAW:${altNonce}:1:alt`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_ALT_REDRAW", nonce: altNonce, trigger: "line" }),
  );
  await page.evaluate(({ id, marker, timeoutMs }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, timeoutMs);
  }, { id: sessionId, marker: altReady, timeoutMs: fixtureArmMs });
  await waitForStableCellFrames(page, sessionId);
  const beforeAlt = await readTerminalStreamProbe(page, sessionId);
  expect(await holdNativeTerminalSelection(page, sessionId)).toBe(true);
  await inputSmokeTerminal(page, sessionId, "passive changed epoch\r");
  const altPending = await waitForCanonicalAdvance(page, sessionId, beforeAlt);
  expectCanonicalAdvanceHeld(beforeAlt, altPending, {
    epoch: "changed",
    readerReason: "selection",
    selectionHold: true,
  });
  expect(altPending.browser.presentation?.mode.canonical?.alt_screen).toBe(true);
  expect(altPending.browser.presentation?.mode.reconciled?.alt_screen).toBe(false);
  const hiddenAltMarker = await attemptPaintedMarker(page, sessionId, altMarker);
  expect(hiddenAltMarker.proof).toBeNull();
  expect(await page.evaluate(() => window.getSelection()?.isCollapsed ?? true)).toBe(false);
  expect((await readTerminalStreamProbe(page, sessionId)).browser.presentation)
    .toMatchObject({
      reader_intent: "reading",
      reader_reason: "selection",
      hold_mask: { selection: true, link: false },
    });
  await armImmediateTerminalPaintSample(page, sessionId, "click", { marker: altMarker });
  await composerSend.click();
  const immediateSubmitPaint = await readImmediateTerminalPaintSample(page);
  expect(immediateSubmitPaint).toMatchObject({
    eventType: "click",
    trusted: true,
    selectionCollapsed: true,
  });
  expect(immediateSubmitPaint?.markerRowRect).not.toBeNull();
  expect(immediateSubmitPaint?.composerHeight).toBeGreaterThanOrEqual(grownBeforePendingSubmit.height - 1);
  const altRecoveredProof = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: altMarker });
  expect(altRecoveredProof).toMatchObject({ marker: altMarker, frames: 2 });
  await expect(composerInput).toHaveValue("");
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? Number.POSITIVE_INFINITY)
    .toBeLessThanOrEqual(restingComposer.height + 1);
  expectRecoveredLive(altPending, await readTerminalStreamProbe(page, sessionId));

  // Reverse the ordering: hold fixture output behind a deterministic delay so
  // accepted admission clears and shrinks the composer before the response.
  // The later marker must still paint immediately because live intent persists.
  const overwriteNonce = `overwrite-${suffix}`;
  const overwriteReady = `ARMED:LINE_OVERWRITE:${overwriteNonce}`;
  const overwriteMarker = `OVERWRITE:${overwriteNonce}:1`;
  await inputSmokeTerminal(
    page,
    sessionId,
    encodePtyFixtureCommand({ op: "ARM_LINE_OVERWRITE", nonce: overwriteNonce }),
  );
  await page.evaluate(({ id, marker, timeoutMs }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, timeoutMs);
  }, { id: sessionId, marker: overwriteReady, timeoutMs: fixtureArmMs });
  const delayedDraft = `delayed-response-${suffix}-` + "payload ".repeat(80);
  await composerInput.fill(delayedDraft);
  await expect.poll(async () => (await composerDock.boundingBox())?.height ?? 0)
    .toBeGreaterThan(restingComposer.height + 1);
  await inputSmokeTerminal(
    page,
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
  const beforeDelayedResponse = await attemptPaintedMarker(page, sessionId, overwriteMarker, 250);
  expect(beforeDelayedResponse.proof).toBeNull();
  const overwritePainted = await page.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: overwriteMarker });
  expect(overwritePainted).toMatchObject({ marker: overwriteMarker, frames: 2 });
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    const { view, replica } = probe.browser;
    const coordinator = coordinatorTerminalViewState(probe);
    return view.status === "accepted"
      && view.active
      && replica.baseline_ready
      && replica.expected_stream_id === view.stream_id
      && coordinator?.activeViews === 1
      && coordinator.streamId === view.stream_id
      && coordinator.effective?.cols === view.effective_cols
      && coordinator.effective?.rows === view.effective_rows;
  }, { timeout: 10_000, intervals: [100] }).toBe(true);
  const presented = await readTerminalStreamProbe(page, sessionId);
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
  expect(presented.worker.worker_fp).toBe(fixtureWorkerFp);
  expect(presented.worker.build).toHaveProperty("git_sha");
  return { presented, overwriteMarker };
}
