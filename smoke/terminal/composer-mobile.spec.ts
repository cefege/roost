import { test, expect } from "./fixtures.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  resetTerminalInputCapture,
  readTerminalInputCapture,
} from "./terminal-helpers.ts";

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
