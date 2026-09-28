import { expect, test } from "./fixtures.ts";
import {
  inputSmokeTerminal,
  navigateToSmokeSession,
  spawnSmokeShell,
  waitForStableCellFrames,
} from "./terminal-helpers.ts";

test("rh probe off-bottom reader", async ({ smokePage, stack }) => {
  test.setTimeout(120_000);
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  await expect(slot).toBeVisible();
  await inputSmokeTerminal(
    smokePage,
    sessionId,
    "for i in $(seq 1 1500); do printf 'READERLINE-%04d stable-history\\n' $i; done\r",
  );
  await expect.poll(() => smokePage.evaluate((id) => {
    const smoke = (window as unknown as { __smoke: { markerScan(id: string, prefix: string): { max: number } } }).__smoke;
    return smoke.markerScan(id, "READERLINE-").max;
  }, sessionId), { timeout: 60_000 }).toBe(1500);
  await inputSmokeTerminal(smokePage, sessionId, "stty -echo\r");
  await waitForStableCellFrames(smokePage, sessionId);
  const grid = slot.locator(".wterm.cell-grid");
  const box = await grid.boundingBox();
  if (!box) throw new Error("grid missing");
  await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await smokePage.mouse.wheel(0, -6000);
  for (let idx = 0; idx < 20; idx++) {
    const state = await smokePage.evaluate((id) => {
      // Throwaway probe: the smoke API is typed in the web bundle, not here.
      const smoke = (window as unknown as { __smoke: Record<string, (...args: unknown[]) => unknown> }).__smoke;
      const container = document.querySelector(`[data-testid="terminal-slot-${id}"] .wterm.cell-grid`) as HTMLElement;
      const rect = container.getBoundingClientRect();
      const row = document.elementFromPoint(rect.left + 100, rect.top + 200)?.closest(".cell-row");
      const browser = smoke.terminalBrowserSnapshot(id) as { presentation?: { reader_intent?: string } };
      const scan = smoke.markerScan(id, "READERLINE-") as { min: number; max: number };
      return {
        scrollTop: container.scrollTop,
        scrollHeight: container.scrollHeight,
        clientHeight: container.clientHeight,
        row: row?.textContent?.slice(0, 40) ?? "",
        intent: browser.presentation?.reader_intent,
        min: scan.min,
        max: scan.max,
        backfill: smoke.scrollbackBackfillRequestCount(id),
        frames: smoke.cellFrameCount(id),
        fulls: smoke.cellFullFrameCount(id),
      };
    }, sessionId);
    console.log("RHPROBE", JSON.stringify(state));
    await smokePage.waitForTimeout(500);
  }
  throw new Error("RHPROBE keep artifacts");
});
