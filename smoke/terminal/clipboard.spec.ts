// Copy/paste ergonomics. The load-bearing part is the multi-line paste guard:
// with bracketed paste OFF, a pasted script runs line by line as it arrives, so
// the payload must be confirmed before a single byte reaches the PTY. Cancelling
// must send NOTHING — that is the whole point of the prompt.

import { test, expect } from "./fixtures.ts";

type ClipSmoke = {
  spawnShell(worker: string, folder: string): Promise<{ session_id: string }>;
  viewportText(sessionId: string): string;
};

test("multi-line paste into an unbracketed shell is confirmed before it is sent", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard + clipboard contract");
  test.setTimeout(120_000);
  await smokePage.context().grantPermissions(["clipboard-read", "clipboard-write"]);

  const sessionId = await smokePage.evaluate(async (workerFp) => {
    const smoke = (window as unknown as Window & { __smoke: ClipSmoke }).__smoke;
    return (await smoke.spawnShell(workerFp, "/tmp")).session_id;
  }, stack.workerFp);
  await smokePage.goto(`${stack.baseUrl}/s/${sessionId}`);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();

  // `cat -v` makes the pasted bytes visible AND takes readline out of the
  // picture, so the session reports bracketed paste off — the mode the guard
  // exists for. Wait for the marker so the frame carrying that mode has landed.
  await smokePage.keyboard.type("cat -v");
  await smokePage.keyboard.press("Enter");
  await smokePage.keyboard.type("BRACKETLESS");
  await smokePage.keyboard.press("Enter");
  await expect.poll(() => slot.textContent()).toContain("BRACKETLESS");

  const payload = "PASTEONE\nPASTETWO\nPASTETHREE";
  await smokePage.evaluate((text) => navigator.clipboard.writeText(text), payload);

  // Cancel: the dialog names the line count and nothing reaches the PTY.
  await smokePage.keyboard.press("Control+Shift+V");
  const cancel = smokePage.getByTestId("paste-guard-cancel");
  await expect(cancel).toBeVisible();
  await expect(smokePage.getByTestId("paste-guard-send")).toContainText("Paste 3 lines");
  await cancel.click();
  await smokePage.waitForTimeout(500);
  expect(await slot.textContent()).not.toContain("PASTEONE");

  // Confirm: the same payload now goes through.
  await smokePage.keyboard.press("Control+Shift+V");
  const send = smokePage.getByTestId("paste-guard-send");
  await expect(send).toBeVisible();
  await send.click();
  await expect.poll(() => slot.textContent(), { timeout: 15_000 }).toContain("PASTETHREE");

  await smokePage.keyboard.press("Control+C");
});
