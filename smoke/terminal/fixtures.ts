import { test as base, expect, devices, type Browser, type Page, type TestInfo } from "@playwright/test";
import { existsSync, readFileSync } from "node:fs";
import { startTerminalTestStack, type TerminalTestStack, type TerminalTestWorker } from "./stack.ts";

type Fixtures = {
  smokePage: Page;
  /** A second page in its own browser context for cross-browser ownership proofs. */
  secondSmokePage: Page;
  /** Separate browser enrollment with the second active dashboard selected. */
  secondDashboardSmokePage: Page;
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
  client?: TerminalTestStack["client"];
  dashboardId?: string;
};

const { defaultBrowserType: _defaultBrowserType, ...iphone15 } = devices["iPhone 15"];

/**
 * The selector only renders from the server-confirmed AuthDashboardAccess
 * snapshot. Waiting for it avoids treating fragment scrubbing as completed
 * browser enrollment: the dispatcher clears `#pair` before its redeem/reload
 * has established the selected dashboard.
 */
export async function waitForConfirmedDashboardScope(
  page: Page,
  expectedDashboardId?: string,
): Promise<void> {
  await page.waitForFunction(
    (expected) =>
      Array.from(
        document.querySelectorAll<HTMLSelectElement>('[data-testid="dashboard-selector"]'),
      ).some((selector) => selector.value !== "" && (expected === null || selector.value === expected)),
    expectedDashboardId ?? null,
  );
}

async function enrollDashboardBrowser(
  page: Page,
  stack: TerminalTestStack,
  client = stack.client,
  dashboardId = stack.dashboardId,
): Promise<void> {
  const token = (await client.authMintBootstrap({
    kind: "browser",
    label: "roost-terminal-test-browser",
  })).token;
  await page.goto(`${stack.baseUrl}/#pair=${encodeURIComponent(token)}`, {
    waitUntil: "domcontentloaded",
  });
  await page.waitForFunction(() => location.hash === "");
  await waitForConfirmedDashboardScope(page, dashboardId);
}

async function useSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
  options: SmokePageOptions = {},
): Promise<void> {
  const dashboardId = options.dashboardId ?? stack.dashboardId;
  const client = options.client ?? stack.client;
  const expectedWorkerFps = options.expectedWorkerFps ?? [stack.workerFp];
  const context = await browser.newContext(options.contextOptions);
  await context.addInitScript((selectedDashboardId) => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    localStorage.setItem("roost.dashboardId", selectedDashboardId);
  }, dashboardId);
  const page = await context.newPage();
  try {
    await enrollDashboardBrowser(page, stack, client, dashboardId);
    await page.waitForFunction(() => typeof window.__smoke === "object");
    await page.waitForFunction(
      (workerFps) => workerFps.every(
        (workerFp) => !!window.__smoke.state().workers[workerFp],
      ),
      expectedWorkerFps,
    );
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
      if (existsSync(stack.secondDashboardPtyFixtureWorkerLogPath)) {
        await testInfo.attach("second-dashboard-pty-fixture-worker.log", {
          body: readFileSync(stack.secondDashboardPtyFixtureWorkerLogPath),
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
  await enrollDashboardBrowser(enrollmentPage, stack);
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
  secondDashboardSmokePage: async ({ browser, stack }, use, testInfo) => {
    const worker = await stack.startSecondDashboardPtyFixtureWorker();
    await useSmokePage(browser, stack, use, testInfo, {
      client: stack.secondDashboardClient,
      dashboardId: stack.secondDashboardId,
      expectedWorkerFps: [worker.workerFp],
    });
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
