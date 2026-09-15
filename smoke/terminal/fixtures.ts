import { test as base, expect, devices, type Browser, type Page, type TestInfo } from "@playwright/test";
import { readFileSync } from "node:fs";
import { startTerminalTestStack, type TerminalTestStack, type TerminalTestWorker } from "./stack.ts";

type Fixtures = {
  smokePage: Page;
  /** A second page in its own browser context for cross-browser ownership proofs. */
  secondSmokePage: Page;
  /** Fresh about:blank context: no Roost HTML/assets/modules have loaded. */
  coldSmokePage: Page;
  mobileSmokePage: Page;
  multiWorkerSmokePage: Page;
  tvSmokePage: Page;
};

type WorkerFixtures = {
  stack: TerminalTestStack;
  secondWorker: TerminalTestWorker;
};

type SmokePageOptions = {
  contextOptions?: Parameters<Browser["newContext"]>[0];
  expectedWorkerFps?: readonly string[];
  /** Force TV mode on before first paint (lib/tvMode.ts reads this at boot). */
  tvMode?: boolean;
};

const { defaultBrowserType: _defaultBrowserType, ...iphone15 } = devices["iPhone 15"];

// Keep default test-time headroom for teardown and service-log attachments.
const SMOKE_BROWSER_READINESS_TIMEOUT_MS = 90_000;

function remainingSmokeReadinessMs(readinessDeadline: number): number {
  const remaining = readinessDeadline - Date.now();
  if (remaining <= 0) {
    throw new Error(`smoke browser readiness timed out after ${SMOKE_BROWSER_READINESS_TIMEOUT_MS}ms`);
  }
  return remaining;
}

async function waitForSmokeWorkers(
  page: Page,
  workerFps: readonly string[],
  readinessDeadline: number,
): Promise<void> {
  await page.waitForFunction(
    (expectedWorkerFps) => expectedWorkerFps.every(
      (workerFp) => !!window.__smoke?.state().workers[workerFp],
    ),
    workerFps,
    { timeout: remainingSmokeReadinessMs(readinessDeadline) },
  );
}

function isMissingFileError(error: unknown): error is NodeJS.ErrnoException {
  return typeof error === "object"
    && error !== null
    && "code" in error
    && error.code === "ENOENT";
}

async function attachStackLog(
  testInfo: TestInfo,
  attachmentName: string,
  logPath: string,
  required: boolean,
): Promise<void> {
  let body: Buffer | undefined;
  try {
    body = readFileSync(logPath);
  } catch (error) {
    if (!isMissingFileError(error)) throw error;
  }
  if (body === undefined) {
    if (required) {
      await testInfo.attach(`${attachmentName}.unavailable`, {
        body: `Stack log was unavailable during fixture teardown: ${logPath}`,
        contentType: "text/plain",
      });
    }
    return;
  }
  await testInfo.attach(attachmentName, { body, contentType: "text/plain" });
}

async function attachStackLogs(testInfo: TestInfo, stack: TerminalTestStack): Promise<void> {
  await attachStackLog(testInfo, "coord.log", stack.coordLogPath, true);
  await attachStackLog(testInfo, "worker.log", stack.workerLogPath, true);
  await attachStackLog(testInfo, "pty-fixture-worker.log", stack.ptyFixtureWorkerLogPath, false);
  await attachStackLog(
    testInfo,
    "second-pty-fixture-worker.log",
    stack.secondPtyFixtureWorkerLogPath,
    false,
  );
  await attachStackLog(testInfo, "second-worker.log", stack.secondWorkerLogPath, false);
}

/**
 * Redeem a pairing token in this page's browser context. Fragment scrubbing is
 * not enrollment on its own: the dispatcher clears `#pair` before its redeem
 * reload has produced an authenticated client, so this waits until the enrolled
 * page actually serves install state — smoke runtime installed, the primary
 * worker routable, the folder list painted, and no error boundary. Enrollment
 * has one deadline so a broken stack fails before fixture teardown loses its
 * service-log window. Every wait reads `window.__smoke` optionally: that reload
 * lands mid-wait, and a document whose smoke chunk has not been imported yet
 * must poll again, not throw.
 */
export async function enrollSmokeBrowser(
  page: Page,
  stack: TerminalTestStack,
  client = stack.client,
  readinessDeadline = Date.now() + SMOKE_BROWSER_READINESS_TIMEOUT_MS,
): Promise<void> {
  const token = (await client.authMintBootstrap({
    kind: "browser",
    label: "roost-terminal-test-browser",
  }, { timeoutMs: remainingSmokeReadinessMs(readinessDeadline) })).token;
  await page.goto(`${stack.baseUrl}/#pair=${encodeURIComponent(token)}`, {
    waitUntil: "domcontentloaded",
    timeout: remainingSmokeReadinessMs(readinessDeadline),
  });
  await page.waitForFunction(
    () => location.hash === "",
    undefined,
    { timeout: remainingSmokeReadinessMs(readinessDeadline) },
  );
  await waitForSmokeWorkers(page, [stack.workerFp], readinessDeadline);
  const workbenchShell = page.locator(".workbench-shell[data-compact]");
  await expect(workbenchShell).toHaveAttribute(
    "data-compact",
    /^(?:true|false)$/,
    { timeout: remainingSmokeReadinessMs(readinessDeadline) },
  );
  const compactState = await workbenchShell.getAttribute("data-compact");
  if (compactState !== "true" && compactState !== "false") {
    throw new Error("workbench shell data-compact must be true or false");
  }
  const compactLayout = compactState === "true";
  const folderList = page.getByTestId("folder-list");
  await expect(folderList).toHaveCount(1, {
    timeout: remainingSmokeReadinessMs(readinessDeadline),
  });
  if (!compactLayout) {
    await expect(folderList).toBeVisible({
      timeout: remainingSmokeReadinessMs(readinessDeadline),
    });
  }
  await expect(page.getByTestId("error-boundary")).toHaveCount(0, {
    timeout: remainingSmokeReadinessMs(readinessDeadline),
  });
}

async function useSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
  options: SmokePageOptions = {},
): Promise<void> {
  const expectedWorkerFps = options.expectedWorkerFps ?? [stack.workerFp];
  const readinessDeadline = Date.now() + SMOKE_BROWSER_READINESS_TIMEOUT_MS;
  const context = await browser.newContext(options.contextOptions);
  let page: Page | undefined;
  let setupComplete = false;
  try {
    await context.addInitScript(() => {
      localStorage.setItem("roostSmoke", "1");
      localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
      if (!sessionStorage.getItem("roost.sidebarViewSeeded")) {
        localStorage.setItem("roost.sidebarView", "spaces");
        localStorage.setItem("roost.sidebarCollapsed", "0");
        sessionStorage.setItem("roost.sidebarViewSeeded", "1");
      }
    });
    // Seeded, not detected: the spec asserts what TV mode DOES, and a UA/pointer
    // heuristic in the assertion path would make a detection miss look like a
    // navigation bug.
    if (options.tvMode) {
      await context.addInitScript(() => {
        localStorage.setItem("roost.tvMode", "on");
      });
    }
    page = await context.newPage();
    await enrollSmokeBrowser(page, stack, stack.client, readinessDeadline);
    await waitForSmokeWorkers(page, expectedWorkerFps, readinessDeadline);
    setupComplete = true;
    await use(page);
  } finally {
    // Setup errors have not reached Playwright's status tracker yet.
    if (!setupComplete || testInfo.status !== testInfo.expectedStatus) {
      await attachStackLogs(testInfo, stack);
    }
    if (page) {
      await page.evaluate(async () => {
        const smoke = window.__smoke;
        smoke?.forceVisible(false);
        await smoke?.cleanupCreated();
      }).catch(() => undefined);
    }
    await context.close();
  }
}

async function useColdSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  use: (page: Page) => Promise<void>,
  testInfo: TestInfo,
): Promise<void> {
  const readinessDeadline = Date.now() + SMOKE_BROWSER_READINESS_TIMEOUT_MS;
  const context = await browser.newContext();
  let page: Page | undefined;
  let setupComplete = false;
  try {
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
    await enrollSmokeBrowser(enrollmentPage, stack, stack.client, readinessDeadline);
    await enrollmentPage.close();
    page = await context.newPage();
    setupComplete = true;
    await use(page);
  } finally {
    // Setup errors have not reached Playwright's status tracker yet.
    if (!setupComplete || testInfo.status !== testInfo.expectedStatus) {
      await attachStackLogs(testInfo, stack);
    }
    if (page) {
      await page.evaluate(async () => {
        const smoke = window.__smoke;
        smoke?.forceVisible(false);
        await smoke?.cleanupCreated();
      }).catch(() => undefined);
    }
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
  tvSmokePage: async ({ browser, stack }, use, testInfo) => {
    await useSmokePage(browser, stack, use, testInfo, {
      contextOptions: { viewport: { width: 1920, height: 1080 }, hasTouch: false },
      tvMode: true,
    });
  },
});

export { expect };
