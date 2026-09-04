import { test, expect } from "./fixtures.ts";
import {
  pressPlatformShortcut,
  spawnSmokeShell,
  navigateToSmokeSession,
} from "./terminal-helpers.ts";

test("desktop active terminal keeps a permanent reserved composer after send", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop composer contract");
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
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop pane composer contract");
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
