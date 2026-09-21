// The controller contract, which only a real stack can show whole: pad mode is
// applied before paint, a D-pad press moves DOM focus, a stick scrolls real
// scrollback on a device that emits NO trusted scroll event and the reader park
// self-resumes at the tail, a shoulder button cycles the pane's tabs, and the
// legend appears. Symptom this pins: "the controller does nothing".

import { test, expect } from "./fixtures.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  inputSmokeTerminal,
  readRenderProbe,
} from "./terminal-helpers.ts";

const BUTTON_A = 0;
const BUTTON_B = 1;
const BUTTON_X = 2;
const BUTTON_Y = 3;
const BUTTON_RB = 5;
const BUTTON_DOWN = 13;
const RIGHT_STICK_Y = 3;

test("a game controller moves focus, scrolls scrollback, activates sidebar search, and cycles tabs @pad", async ({
  padSmokePage,
  stack,
}, testInfo) => {
  // WebKit ships no Gamepad API, and macOS CI's webkit-iphone project only
  // grep-inverts @serial|@tv, so without this it would run there and fail.
  test.skip(!testInfo.project.name.startsWith("chromium"), "Gamepad API contract");
  await expect(padSmokePage.locator("html")).toHaveAttribute("data-pad", "true");

  const focusSignature = () =>
    padSmokePage.evaluate(() => {
      const active = document.activeElement as HTMLElement | null;
      if (!active) return "none";
      return `${active.tagName}:${active.dataset.testid ?? active.id ?? ""}`;
    });

  // The adapter polls in requestAnimationFrame, so a press must survive at
  // least one frame before release or the poll never observes it.
  const settleFrames = (count: number) =>
    padSmokePage.evaluate(
      (frames) =>
        new Promise<void>((resolve) => {
          let remaining = frames;
          const step = () => {
            if (remaining-- <= 0) return resolve();
            requestAnimationFrame(step);
          };
          step();
        }),
      count,
    );

  const setButton = (index: number, pressed: boolean) =>
    padSmokePage.evaluate(({ index: idx, pressed: down }) => {
      const pad = window.__fakePad;
      if (!pad) throw new Error("fake pad was not installed");
      pad.buttons[idx].pressed = down;
    }, { index, pressed });

  const pressPadButton = async (index: number) => {
    await setButton(index, true);
    await settleFrames(3);
    await setButton(index, false);
    await settleFrames(2);
  };

  const holdAxis = (axis: number, value: number) =>
    padSmokePage.evaluate(({ axis: which, value: amount }) => {
      const pad = window.__fakePad;
      if (!pad) throw new Error("fake pad was not installed");
      pad.axes[which] = amount;
    }, { axis, value });

  // ── Connecting the pad AFTER load is the real flow ──────────────────────
  // The fixture reports no pads until __attachFakePad(), so this is the only
  // path through gamepadSource's gamepadconnected → refreshPads branch: before
  // it there is no poll at all, and a held button must move nothing.
  const activitySessions = padSmokePage.locator("#workbench-activity-sessions");
  await expect(activitySessions).toBeVisible();
  await activitySessions.focus();
  const activityOrigin = await focusSignature();
  await pressPadButton(BUTTON_DOWN);
  expect(await focusSignature()).toBe(activityOrigin);

  await padSmokePage.evaluate(() => {
    const attach = window.__attachFakePad;
    if (!attach) throw new Error("fake pad attach hook was not installed");
    attach();
  });
  await pressPadButton(BUTTON_DOWN);
  expect(await focusSignature()).not.toBe(activityOrigin);

  // ── Scrollback under the right stick ────────────────────────────────────
  const sessionId = (await spawnSmokeShell(padSmokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(padSmokePage, sessionId);
  await waitForStableCellFrames(padSmokePage, sessionId);
  await inputSmokeTerminal(padSmokePage, sessionId, "seq 1 400\n");
  await expect
    .poll(() => padSmokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .toContain("400");
  await waitForStableCellFrames(padSmokePage, sessionId);

  const display = padSmokePage.getByTestId("terminal-display");
  // tabindex="0" exists only in a directional modality; without it the pad can
  // never put focus on the scroll box at all.
  await expect(display).toHaveAttribute("tabindex", "0");
  await display.focus();
  await expect(display).toBeFocused();
  expect((await readRenderProbe(padSmokePage, sessionId)).atBottom).toBe(true);

  await holdAxis(RIGHT_STICK_Y, -1);
  await expect
    .poll(async () => (await readRenderProbe(padSmokePage, sessionId)).atBottom)
    .toBe(false);
  expect((await readRenderProbe(padSmokePage, sessionId)).fromBottom).toBeGreaterThan(0);
  await holdAxis(RIGHT_STICK_Y, 0);

  await holdAxis(RIGHT_STICK_Y, 1);
  await expect
    .poll(async () => (await readRenderProbe(padSmokePage, sessionId)).atBottom)
    .toBe(true);
  await holdAxis(RIGHT_STICK_Y, 0);

  // ── Shoulder buttons cycle the pane's tabs ──────────────────────────────
  const siblingId = (await spawnSmokeShell(padSmokePage, stack.workerFp)).session_id;
  await expect(padSmokePage.getByTestId(`tab-${siblingId}`)).toBeVisible();
  await navigateToSmokeSession(padSmokePage, sessionId);
  await expect(padSmokePage.getByTestId(`tab-${sessionId}`)).toHaveAttribute(
    "data-active",
    "true",
  );
  await pressPadButton(BUTTON_RB);
  await expect(padSmokePage.getByTestId(`tab-${siblingId}`)).toHaveAttribute(
    "data-active",
    "true",
  );

  // ── Y opens the focused row's menu; B leaves focus where it started ─────
  // Roving focus is what makes such a menu reachable at all: CtxMenuItem is
  // tabIndex=-1, so only programmatic focus can enter it, and a pad that
  // cannot get back out of it strands the user.
  const folderRow = padSmokePage.locator(".df-row__primary").first();
  await folderRow.focus();
  await pressPadButton(BUTTON_Y);
  const rowMenu = padSmokePage.getByRole("menu", { name: "Workspace actions" });
  await expect(rowMenu).toBeVisible();
  await expect(rowMenu.getByRole("menuitem").first()).toBeFocused();
  await pressPadButton(BUTTON_B);
  await expect(padSmokePage.getByTestId("folder-context-menu")).toHaveCount(0);
  await expect(folderRow).toBeFocused();

  // ── Overlays: X opens the palette, B closes it ──────────────────────────
  await pressPadButton(BUTTON_X);
  await expect(padSmokePage.getByTestId("command-palette")).toBeVisible();
  await pressPadButton(BUTTON_B);
  await expect(padSmokePage.getByTestId("command-palette")).toHaveCount(0);

  // ── The legend names the current bindings ───────────────────────────────
  await pressPadButton(BUTTON_DOWN);
  await expect(padSmokePage.getByTestId("pad-hint-bar")).toBeVisible();
  // ── Sidebar search: A enters, B unwinds before closing the drawer ────────
  await padSmokePage.setViewportSize({ width: 500, height: 800 });
  const drawer = padSmokePage.getByTestId("sidebar-drawer");
  await expect(padSmokePage.getByTestId("mobile-deck-bar-menu")).toBeVisible();
  await padSmokePage.getByTestId("mobile-deck-bar-menu").click();
  await expect(drawer).toHaveAttribute("data-open", "true");

  const sidebarSearch = drawer.getByTestId("sidebar-search");
  const searchTrigger = drawer.getByTestId("sidebar-search-trigger");
  await searchTrigger.focus();
  await pressPadButton(BUTTON_A);
  await expect(sidebarSearch).toBeFocused();

  await sidebarSearch.fill("keep-input-focused");
  await pressPadButton(BUTTON_B);
  await expect(sidebarSearch).toHaveValue("");
  await expect(sidebarSearch).toBeFocused();
  await pressPadButton(BUTTON_B);
  await expect(searchTrigger).toBeFocused();
  await pressPadButton(BUTTON_B);
  await expect(drawer).toHaveAttribute("data-open", "false");
});
