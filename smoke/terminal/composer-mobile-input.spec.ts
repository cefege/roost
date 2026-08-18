import { test, expect } from "./fixtures.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  switchToSmokeSession,
  expectSmokeComposer,
  inputSmokeTerminal,
} from "./terminal-helpers.ts";

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
