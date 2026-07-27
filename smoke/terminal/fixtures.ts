import { test as base, expect, type Page } from "@playwright/test";
import { readFileSync } from "node:fs";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";

type Fixtures = {
  smokePage: Page;
};

type WorkerFixtures = {
  stack: TerminalTestStack;
};

export const test = base.extend<Fixtures, WorkerFixtures>({
  stack: [async ({}, use) => {
    const stack = await startTerminalTestStack();
    try {
      await use(stack);
    } finally {
      await stack.stop();
    }
  }, { scope: "worker" }],
  smokePage: async ({ browser, stack }, use, testInfo) => {
    const context = await browser.newContext();
    await context.addInitScript(() => {
      localStorage.setItem("roostSmoke", "1");
      localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    });
    const page = await context.newPage();
    try {
      await page.goto(stack.baseUrl, { waitUntil: "domcontentloaded" });
      await page.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
      await page.waitForFunction((workerFp) => {
        const smoke = (window as unknown as Window & { __smoke: { state(): { workers: Record<string, unknown> } } }).__smoke;
        return !!smoke.state().workers[workerFp];
      }, stack.workerFp);
      await expect(page.getByTestId("folder-list")).toBeVisible();
      await expect(page.getByTestId("error-boundary")).toHaveCount(0);
      await use(page);
    } finally {
      if (testInfo.status !== testInfo.expectedStatus) {
        await testInfo.attach("coord.log", { body: readFileSync(stack.coordLogPath), contentType: "text/plain" });
        await testInfo.attach("worker.log", { body: readFileSync(stack.workerLogPath), contentType: "text/plain" });
      }
      await page.evaluate(async () => {
        const smoke = (window as unknown as Window & { __smoke?: { forceVisible(on: boolean): void; cleanupCreated(): Promise<unknown> } }).__smoke;
        smoke?.forceVisible(false);
        await smoke?.cleanupCreated();
      }).catch(() => undefined);
      await context.close();
    }
  },
});

export { expect };
