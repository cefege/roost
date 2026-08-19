import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnPtyFixtureSession,
  navigateToSmokeSession,
  expectSmokeComposer,
  inputSmokeTerminal,
  resetTerminalInputCapture,
  readTerminalInputCapture,
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
} from "./terminal-probe-helpers.ts";
import {
  measureMobileWheelTarget,
  mobilePaneFocused,
  readMobileKeyboardGeometry,
} from "./composer-mobile-keyboard-probes.ts";

test("mobile terminal keyboard toggles and dispatches special keys", async ({ mobileSmokePage, stack }, testInfo) => {
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
  const paneFocused = () => mobilePaneFocused(mobileSmokePage, sessionId);

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

  const readGeometry = () => readMobileKeyboardGeometry(mobileSmokePage, sessionId);
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

  // Both wheel gestures below need a TRUSTED wheel, and `mouse.wheel` is
  // unsupported in mobile WebKit (Playwright driver limit, not a product carve-out).
  if (testInfo.project.name.startsWith("chromium")) {
    // NEGATIVE: mouse-indifferent app, toggle ON, trusted wheel — nothing forwards.
    const nativeWheelTarget = await measureMobileWheelTarget(terminalSlot);
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
