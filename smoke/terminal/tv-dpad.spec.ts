// The D-pad contract for a TV browser, which only a real stack can show whole:
// TV mode is applied before paint, the four arrows move DOM focus, a focused
// .wterm scrolls real scrollback and returns to the live tail, and /pair keeps
// its own arrow keys. Symptom this pins: "I can't scroll" on a smart-TV remote.

import { test, expect } from "./fixtures.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  waitForStableCellFrames,
  inputSmokeTerminal,
  pressPlatformShortcut,
} from "./terminal-helpers.ts";

declare global {
  interface Window {
    /** Set by this spec's in-page listener: did the app cancel the arrow key?
     *  Only a page-side listener can observe defaultPrevented. */
    __tvArrowCancelled?: boolean;
  }
}

test("TV mode navigates, scrolls scrollback, and explicitly activates sidebar search @tv", async ({
  tvSmokePage,
  stack,
}) => {
  await expect(tvSmokePage.locator("html")).toHaveAttribute("data-tv", "true");

  const focusSignature = () =>
    tvSmokePage.evaluate(() => {
      const active = document.activeElement as HTMLElement | null;
      if (!active) return "none";
      return `${active.tagName}:${active.dataset.testid ?? active.id ?? ""}`;
    });

  const session = await spawnSmokeShell(tvSmokePage, stack.workerFp);
  const sessionId = session.session_id;
  await navigateToSmokeSession(tvSmokePage, sessionId);
  await waitForStableCellFrames(tvSmokePage, sessionId);

  // ── Scrollback under the D-pad ──────────────────────────────────────────
  await inputSmokeTerminal(tvSmokePage, sessionId, "seq 1 400\n");
  await expect
    .poll(() => tvSmokePage.getByTestId(`terminal-slot-${sessionId}`).textContent())
    .toContain("400");
  await waitForStableCellFrames(tvSmokePage, sessionId);

  const display = tvSmokePage.getByTestId("terminal-display");
  // tabindex="0" exists only in TV mode; without it the remote can never put
  // focus on the scroll box at all, which is the whole "I can't scroll" report.
  await expect(display).toHaveAttribute("tabindex", "0");
  await display.focus();
  await expect(display).toBeFocused();

  const readProbe = () =>
    tvSmokePage.evaluate((id) => window.__smoke.renderProbe(id), sessionId);
  expect((await readProbe()).atBottom).toBe(true);

  for (let press = 0; press < 5; press += 1) {
    await tvSmokePage.keyboard.press("ArrowUp");
  }
  await expect.poll(async () => (await readProbe()).atBottom).toBe(false);
  expect((await readProbe()).fromBottom).toBeGreaterThan(0);
  // Focus stays on the scroll box while it can still travel: the arrows belong
  // to the scroller until it clamps, and only then to focus navigation.
  await expect(display).toBeFocused();

  for (let press = 0; press < 40; press += 1) {
    if ((await readProbe()).atBottom) break;
    await tvSmokePage.keyboard.press("ArrowDown");
  }
  expect((await readProbe()).atBottom).toBe(true);

  // ── Overscan: the portaled key pad must clear the cropped bezel ──────────
  // .term-nav and its toggle are position:fixed on <body>, so they escape
  // .workbench-shell's overscan padding and their env(safe-area-inset-*) is
  // 0px on a television. The pad is the TV's only raw-key surface, so a crop
  // here costs Esc/Tab/Ctrl entirely.
  const overscan = await tvSmokePage.evaluate(() => {
    const styles = getComputedStyle(document.documentElement);
    return {
      inline: Number.parseFloat(styles.getPropertyValue("--tv-overscan-inline")),
      block: Number.parseFloat(styles.getPropertyValue("--tv-overscan-block")),
      width: window.innerWidth,
      height: window.innerHeight,
    };
  });
  expect(overscan.inline).toBeGreaterThan(0);

  const expectInsideOverscan = async (selector: string) => {
    const box = await tvSmokePage.locator(selector).boundingBox();
    expect(box, `${selector} must be laid out`).not.toBeNull();
    if (!box) return;
    expect(overscan.width - (box.x + box.width)).toBeGreaterThanOrEqual(overscan.inline);
    expect(overscan.height - (box.y + box.height)).toBeGreaterThanOrEqual(overscan.block);
    expect(box.y).toBeGreaterThanOrEqual(overscan.block);
  };

  await expect(tvSmokePage.locator(".term-nav-toggle")).toBeVisible();
  await expectInsideOverscan(".term-nav-toggle");
  await tvSmokePage.locator(".term-nav-toggle").click();
  await expect(tvSmokePage.getByTestId("terminal-nav-buttons")).toBeVisible();
  await expectInsideOverscan(".term-nav");
  await tvSmokePage.locator(".term-nav-toggle").click();

  // ── Directional focus travel ────────────────────────────────────────────
  const activitySessions = tvSmokePage.locator("#workbench-activity-sessions");
  await expect(activitySessions).toBeVisible();

  await activitySessions.focus();
  const activityOrigin = await focusSignature();
  await tvSmokePage.keyboard.press("ArrowDown");
  expect(await focusSignature()).not.toBe(activityOrigin);

  await activitySessions.focus();
  expect(await focusSignature()).toBe(activityOrigin);
  await tvSmokePage.keyboard.press("ArrowRight");
  expect(await focusSignature()).not.toBe(activityOrigin);

  // ── Sidebar search needs deliberate activation ───────────────────────────
  const sidebarSearch = tvSmokePage.getByTestId("sidebar-search");
  const searchTrigger = tvSmokePage.getByTestId("sidebar-search-trigger");
  await expect(searchTrigger).toBeVisible();

  // Directional travel, list edges, and scrolling must never transfer focus to
  // the text field and open a TV keyboard.
  await searchTrigger.focus();
  await tvSmokePage.keyboard.press("ArrowDown");
  await expect(sidebarSearch).not.toBeFocused();
  const firstFolderRow = tvSmokePage.locator(".df-row__primary").first();
  await expect(firstFolderRow).toBeVisible();
  await firstFolderRow.focus();
  await tvSmokePage.keyboard.press("ArrowUp");
  await expect(sidebarSearch).not.toBeFocused();
  await tvSmokePage.mouse.wheel(0, 180);
  await expect(sidebarSearch).not.toBeFocused();

  // Native Enter, Tab, pointer activation, and the platform shortcut retain
  // their desktop behavior while TV navigation requires the trigger first.
  await searchTrigger.focus();
  await tvSmokePage.keyboard.press("Enter");
  await expect(sidebarSearch).toBeFocused();
  await sidebarSearch.fill("only-this-query");
  await tvSmokePage.keyboard.press("Escape");
  await expect(sidebarSearch).toHaveValue("");
  await expect(sidebarSearch).toBeFocused();
  await tvSmokePage.keyboard.press("Escape");
  await expect(searchTrigger).toBeFocused();

  await searchTrigger.focus();
  await tvSmokePage.keyboard.press("Tab");
  await expect(sidebarSearch).toBeFocused();
  await searchTrigger.click();
  await expect(sidebarSearch).toBeFocused();
  // Terminal routes intentionally keep application shortcuts behind PTY
  // ownership. Prove the unchanged platform shortcut on a non-terminal route.
  await tvSmokePage.goto(`${stack.baseUrl}/settings/machines?tv=1`, {
    waitUntil: "domcontentloaded",
  });
  await expect(sidebarSearch).toBeVisible();
  await tvSmokePage.getByRole("button", { name: "Add machine" }).first().focus();
  await pressPlatformShortcut(tvSmokePage, "sidebarSearch", "f");
  await expect(sidebarSearch).toBeFocused();

  // ── /pair: the arrows must not be swallowed ──────────────────────────────
  // This route renders outside the workbench shell and publishes no sidebar
  // cursor rows, so the global router must leave bare ↑/↓/⏎ alone: cancelling
  // them leaves the page unable to scroll and a focused button unable to
  // activate. Failure is "cancelled AND nothing moved" — either a native
  // scroll or a focus move is a legitimate outcome of one press.
  await tvSmokePage.goto(`${stack.baseUrl}/pair?tv=1`, { waitUntil: "domcontentloaded" });
  await expect(tvSmokePage.getByTestId("onboarding")).toBeVisible();
  await expect(tvSmokePage.locator("html")).toHaveAttribute("data-tv", "true");

  await tvSmokePage.evaluate(() => {
    window.__tvArrowCancelled = undefined;
    window.addEventListener(
      "keydown",
      (event) => {
        if (event.key === "ArrowDown") window.__tvArrowCancelled = event.defaultPrevented;
      },
      { once: true },
    );
  });
  const pairOrigin = await focusSignature();
  const scrollBefore = await tvSmokePage.evaluate(() => window.scrollY);
  await tvSmokePage.keyboard.press("ArrowDown");
  const outcome = await tvSmokePage.evaluate(() => ({
    cancelled: window.__tvArrowCancelled,
    scrollY: window.scrollY,
  }));
  expect(outcome.cancelled).not.toBeUndefined();
  const somethingMoved =
    outcome.scrollY > scrollBefore || (await focusSignature()) !== pairOrigin;
  expect(outcome.cancelled === false || somethingMoved).toBe(true);
});
