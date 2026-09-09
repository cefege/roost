import { test as base, expect, devices, type Browser, type Page, type TestInfo } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { startTerminalTestStack, type TerminalTestStack, type TerminalTestWorker } from "./stack.ts";

type Fixtures = {
  smokePage: Page;
  /** A second page in its own browser context for cross-browser ownership proofs. */
  secondSmokePage: Page;
  /** Fresh about:blank context: no Roost HTML/assets/modules have loaded. */
  coldSmokePage: Page;
  mobileSmokePage: Page;
  multiWorkerSmokePage: Page;
};

type WorkerFixtures = {
  stack: TerminalTestStack;
  secondWorker: TerminalTestWorker;
};

type SmokePageOptions = {
  contextOptions?: Parameters<Browser["newContext"]>[0];
  expectedWorkerFps?: readonly string[];
};

const { defaultBrowserType: _defaultBrowserType, ...iphone15 } = devices["iPhone 15"];

/**
 * Redeem a pairing token in this page's browser context. Fragment scrubbing is
 * not enrollment on its own: the dispatcher clears `#pair` before its redeem
 * reload has produced an authenticated client, so this waits until the enrolled
 * page actually serves install state — smoke runtime installed, the primary
 * worker routable, the folder list painted, and no error boundary. Every wait
 * reads `window.__smoke` optionally: that reload lands mid-wait, and a document
 * whose smoke chunk has not been imported yet must poll again, not throw.
 */
export async function enrollSmokeBrowser(
  page: Page,
  stack: TerminalTestStack,
  client = stack.client,
): Promise<void> {
  const token = (await client.authMintBootstrap({
    kind: "browser",
    label: "roost-terminal-test-browser",
  })).token;
  await page.goto(`${stack.baseUrl}/#pair=${encodeURIComponent(token)}`, {
    waitUntil: "domcontentloaded",
  });
  await page.waitForFunction(() => location.hash === "");
  await page.waitForFunction(
    (workerFp) => !!window.__smoke?.state().workers[workerFp],
    stack.workerFp,
  );
  await expect(page.getByTestId("folder-list")).toBeVisible();
  await expect(page.getByTestId("error-boundary")).toHaveCount(0);
}

async function useSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
  options: SmokePageOptions = {},
): Promise<void> {
  const expectedWorkerFps = options.expectedWorkerFps ?? [stack.workerFp];
  const context = await browser.newContext(options.contextOptions);
  await context.addInitScript(() => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
  });
  const page = await context.newPage();
  try {
    await enrollSmokeBrowser(page, stack);
    await page.waitForFunction(
      (workerFps) => workerFps.every(
        (workerFp) => !!window.__smoke?.state().workers[workerFp],
      ),
      expectedWorkerFps,
    );
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
      const smoke = window.__smoke;
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
  const enrollmentPage = await context.newPage();
  await enrollSmokeBrowser(enrollmentPage, stack);
  await enrollmentPage.close();
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
      const smoke = window.__smoke;
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
  secondSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo);
  },
  coldSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useColdSmokePage(browser, stack, use, testInfo);
  },
  multiWorkerSmokePage: async ({ browser, stack, secondWorker }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo, {
      expectedWorkerFps: [stack.workerFp, secondWorker.workerFp],
    });
  },
  mobileSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo, { contextOptions: iphone15 });
  },
});

export { expect };
