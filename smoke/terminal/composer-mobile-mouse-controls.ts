// Mobile mouse controls verify native reader gestures, application forwarding, and rotation.
// They continue the keyboard-control context inside the same composer smoke test.
// Pending terminal paint must reconcile before forwarded wheel bytes reach the PTY.

import type { TestInfo } from "@playwright/test";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  inputSmokeTerminal,
  resetTerminalInputCapture,
  readTerminalInputCapture,
} from "./terminal-helpers.ts";
import {
  attemptPaintedMarker,
  armImmediateTerminalPaintSample,
  readImmediateTerminalPaintSample,
} from "./terminal-paint-helpers.ts";
import {
  readTerminalStreamProbe,
  waitForCanonicalAdvance,
  expectCanonicalAdvanceHeld,
  expectRecoveredLive,
} from "./terminal-probe-helpers.ts";
import { measureMobileWheelTarget } from "./composer-mobile-keyboard-probes.ts";
import type { MobileKeyboardProbeContext } from "./composer-mobile-keyboard-controls.ts";

export async function exerciseMobileMouseAndGeometry(
  context: MobileKeyboardProbeContext,
  testInfo: TestInfo,
): Promise<void> {
  const {
    mobileSmokePage,
    sessionId,
    terminalSlot,
    panel,
    input,
    toggle,
    mouse,
    paneFocused,
    readGeometry,
    rectDelta,
    expectOpenGeometry,
    closedGeometry,
    initialPortrait,
    draft,
  } = context;
  // Alt-screen occupancy is NOT a mouse request. This fixture enters the alt
  // screen without DECSET 1000/1002, exactly like less, man, and plain vim.
  // A clamped native gesture stays native and live; reader intent begins only
  // for a wheel that can actually move through normal-screen history.
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

  // Both wheel gestures below need a TRUSTED wheel, and `mouse.wheel` is
  // unsupported in mobile WebKit (Playwright driver limit, not a product carve-out).
  if (testInfo.project.name.startsWith("chromium")) {
    // A mouse-indifferent alternate screen neither forwards nor starts a
    // reader hold when its browser scroll position is clamped.
    const altWheelTarget = await measureMobileWheelTarget(terminalSlot);
    expect(altWheelTarget).toMatchObject({ col: 1, row: 1 });
    await resetTerminalInputCapture(mobileSmokePage);
    await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "wheel", {});
    await mobileSmokePage.mouse.move(altWheelTarget.x, altWheelTarget.y);
    await mobileSmokePage.mouse.wheel(0, 120);
    expect(await readImmediateTerminalPaintSample(mobileSmokePage)).toMatchObject({
      eventType: "wheel",
      trusted: true,
    });
    const altWheelCapture = await readTerminalInputCapture(mobileSmokePage);
    expect(altWheelCapture.droppedBatches).toBe(0);
    expect(altWheelCapture.batches).toEqual([]);
    await expect.poll(async () => {
      const presentation = (await readTerminalStreamProbe(mobileSmokePage, sessionId))
        .browser.presentation;
      return { intent: presentation?.reader_intent, reason: presentation?.reader_reason };
    }).toEqual({ intent: "live", reason: null });
  }
  await inputSmokeTerminal(
    mobileSmokePage,
    sessionId,
    encodePtyFixtureCommand({ op: "ALT_SCREEN", active: false, nonce: forwardAltNonce }),
  );
  await mobileSmokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: `ALT_EXIT:${forwardAltNonce}` });

  if (testInfo.project.name.startsWith("chromium")) {
    // Populate genuine normal-screen overflow, then wheel toward history from
    // the live tail. This is the native-reader path; it must hold a later
    // canonical frame without admitting a byte to the PTY.
    const readerPrefix = `NATIVE-READER-${forwardAltNonce}-`;
    const readerLineCount = 96;
    await inputSmokeTerminal(
      mobileSmokePage,
      sessionId,
      encodePtyFixtureCommand({
        op: "FLOOD",
        prefix: readerPrefix,
        count: readerLineCount,
      }),
    );
    await mobileSmokePage.evaluate(({ id, marker }) => {
      const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
      return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
    }, { id: sessionId, marker: `${readerPrefix}${readerLineCount}` });
    await expect.poll(() => terminalSlot.locator(".cell-grid").evaluate((grid) => {
      if (!(grid instanceof HTMLElement)) return false;
      return grid.scrollHeight > grid.clientHeight;
    }), { message: "native reader wheel requires normal-screen overflow" }).toBe(true);

    const nativeWheelTarget = await measureMobileWheelTarget(terminalSlot);
    expect(nativeWheelTarget).toMatchObject({ col: 1, row: 1 });
    await resetTerminalInputCapture(mobileSmokePage);
    await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "wheel", {});
    await mobileSmokePage.mouse.move(nativeWheelTarget.x, nativeWheelTarget.y);
    await mobileSmokePage.mouse.wheel(0, -120);
    expect(await readImmediateTerminalPaintSample(mobileSmokePage)).toMatchObject({
      eventType: "wheel",
      trusted: true,
    });
    const nativeWheelCapture = await readTerminalInputCapture(mobileSmokePage);
    expect(nativeWheelCapture.droppedBatches).toBe(0);
    expect(nativeWheelCapture.batches).toEqual([]);
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
    // POSITIVE: the same cell's gesture now forwards after the application asks.
    await resetTerminalInputCapture(mobileSmokePage);
    await armImmediateTerminalPaintSample(mobileSmokePage, sessionId, "wheel", {
      marker: forwardPendingMarker,
    });
    const forwardWheelTarget = await measureMobileWheelTarget(terminalSlot);
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
  }
  await mouse.tap();
  await expect(mouse).toHaveAttribute("aria-pressed", "false");

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
}
