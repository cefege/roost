import { test as base, expect, devices, type Browser, type Page, type TestInfo } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { startTerminalTestStack, type TerminalTestStack, type TerminalTestWorker } from "./stack.ts";

type Fixtures = {
  smokePage: Page;
  /** Fresh about:blank context: no Roost HTML/assets/modules have loaded. */
  coldSmokePage: Page;
  mobileSmokePage: Page;
  multiWorkerSmokePage: Page;
};

type WorkerFixtures = {
  stack: TerminalTestStack;
  secondWorker: TerminalTestWorker;
};

const { defaultBrowserType: _defaultBrowserType, ...iphone15 } = devices["iPhone 15"];

async function useSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
  contextOptions?: Parameters<Browser["newContext"]>[0],
  expectedWorkerFps: readonly string[] = [stack.workerFp],
): Promise<void> {
  const context = await browser.newContext(contextOptions);
  await context.addInitScript(() => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
  });
  const page = await context.newPage();
  try {
    await page.goto(stack.baseUrl, { waitUntil: "domcontentloaded" });
    await page.waitForFunction(() => typeof (window as unknown as Window & { __smoke?: unknown }).__smoke === "object");
    await page.waitForFunction((workerFps) => {
      const smoke = (window as unknown as Window & { __smoke: { state(): { workers: Record<string, unknown> } } }).__smoke;
      return workerFps.every((workerFp) => !!smoke.state().workers[workerFp]);
    }, expectedWorkerFps);
    await expect(page.getByTestId("folder-list")).toBeVisible();
    await expect(page.getByTestId("error-boundary")).toHaveCount(0);
    await use(page);
  } finally {
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach("coord.log", { body: readFileSync(stack.coordLogPath), contentType: "text/plain" });
      await testInfo.attach("worker.log", { body: readFileSync(stack.workerLogPath), contentType: "text/plain" });
      if (existsSync(stack.ptyFixtureWorkerLogPath)) {
        await testInfo.attach("pty-fixture-worker.log", {
          body: readFileSync(stack.ptyFixtureWorkerLogPath),
          contentType: "text/plain",
        });
      }
      if (existsSync(stack.secondWorkerLogPath)) {
        await testInfo.attach("second-worker.log", {
          body: readFileSync(stack.secondWorkerLogPath),
          contentType: "text/plain",
        });
      }
    }
    await page.evaluate(async () => {
      const smoke = (window as unknown as Window & { __smoke?: { forceVisible(on: boolean): void; cleanupCreated(): Promise<unknown> } }).__smoke;
      smoke?.forceVisible(false);
      await smoke?.cleanupCreated();
    }).catch(() => undefined);
    await context.close();
  }
}

async function useColdSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
): Promise<void> {
  const context = await browser.newContext();
  await context.addInitScript(() => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    const driverEpoch = Number(new URL(location.href).searchParams.get("__roost_driver_nav"));
    if (Number.isFinite(driverEpoch) && driverEpoch > 0) {
      (window as Window & { __roostDriverBeforeNavigationEpochMs?: number })
        .__roostDriverBeforeNavigationEpochMs = driverEpoch;
    }
  });
  const page = await context.newPage();
  try {
    await use(page);
  } finally {
    if (testInfo.status !== testInfo.expectedStatus) {
      await testInfo.attach("coord.log", { body: readFileSync(stack.coordLogPath), contentType: "text/plain" });
      await testInfo.attach("worker.log", { body: readFileSync(stack.workerLogPath), contentType: "text/plain" });
      if (existsSync(stack.ptyFixtureWorkerLogPath)) {
        await testInfo.attach("pty-fixture-worker.log", {
          body: readFileSync(stack.ptyFixtureWorkerLogPath),
          contentType: "text/plain",
        });
      }
    }
    await page.evaluate(async () => {
      const smoke = (window as Window & {
        __smoke?: { forceVisible(on: boolean): void; cleanupCreated(): Promise<unknown> };
      }).__smoke;
      smoke?.forceVisible(false);
      await smoke?.cleanupCreated();
    }).catch(() => undefined);
    await context.close();
  }
}


export const test = base.extend<Fixtures, WorkerFixtures>({
  stack: [async ({}, use) => {
    const stack = await startTerminalTestStack();
    try {
      await use(stack);
    } finally {
      await stack.stop();
    }
  }, { scope: "worker" }],
  secondWorker: [async ({ stack }, use) => {
    await use(await stack.startSecondWorker());
  }, { scope: "worker" }],
  smokePage: async ({ browser, stack }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo);
  },
  coldSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useColdSmokePage(browser, stack, use, testInfo);
  },
  multiWorkerSmokePage: async ({ browser, stack, secondWorker }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo, undefined, [stack.workerFp, secondWorker.workerFp]);
  },
  mobileSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo, iphone15);
  },
});

export { expect };
