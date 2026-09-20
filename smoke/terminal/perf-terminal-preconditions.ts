// Performance browser preconditions keep visual shortcuts out of trusted-key timing.
// Both fleet and peer probes reload enrolled pages after setting the persisted prediction preference.
// The helper re-establishes smoke visibility and worker state without adding a transport shortcut.
import type { Page } from "@playwright/test";

export async function disableTerminalPredictions(
  pages: readonly Page[],
  workerFps: readonly string[],
): Promise<void> {
  const contexts = new Set(pages.map((page) => page.context()));
  for (const context of contexts) {
    await context.addInitScript(() => localStorage.setItem("roostPredict", "never"));
  }
  for (const page of pages) await page.evaluate(() => localStorage.setItem("roostPredict", "never"));
  for (const page of pages) {
    await page.reload({ waitUntil: "domcontentloaded" });
    await page.waitForFunction((expectedWorkerFps) => {
      const smokeWindow = window as unknown as {
        __smoke?: { state(): { workers: Record<string, unknown> }; forceVisible(on: boolean): void };
      };
      return typeof smokeWindow.__smoke === "object"
        && expectedWorkerFps.every((workerFp) => !!smokeWindow.__smoke?.state().workers[workerFp]);
    }, [...workerFps]);
    const predictionDisabled = await page.evaluate(() => {
      const smokeWindow = window as unknown as { __smoke: { forceVisible(on: boolean): void } };
      smokeWindow.__smoke.forceVisible(true);
      return localStorage.getItem("roostPredict") === "never";
    });
    if (!predictionDisabled) throw new Error("terminal performance probe could not disable predictive echo");
  }
}
