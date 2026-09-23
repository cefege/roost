/**
 * Real-browser proof for a fresh local-only coordinator profile.
 * The dedicated stack keeps coordinator, worker, keeper, Sync, and PTY wiring real.
 * It pairs through the ordinary fragment flow, then proves expansion guidance never mints a worker grant.
 */

import { execFileSync } from "node:child_process";
import type { BrowserContext } from "@playwright/test";
import { expect, enrollSmokeBrowser, test } from "./fixtures.ts";
import { attachStackLogs } from "./terminal-local-fast-path-helpers.ts";
import { navigateToSmokeSession, spawnSmokeShell, uniqueMarker } from "./terminal-helpers.ts";
import { startTerminalTestStack, type TerminalTestStack } from "./stack.ts";
import { REPOSITORY_ROOT } from "./stack-runtime.ts";

const WORKERS_LIST_PATH = "/roost.v1.CoordinatorService/WorkersList";
const MINT_BOOTSTRAP_PATH = "/roost.v1.CoordinatorService/AuthMintBootstrap";
const COORD_IDENTITY_PATH = "/roost.v1.CoordinatorService/AuthCoordIdentity";
const LOCAL_ONLY_GUIDANCE = [
  "Choose an HTTPS address supplied by the operator's proxy/tunnel/private-access setup.",
  "Before exposing the running local listener, run roost quickstart --coordinator-url https://<your-Roost-address>",
  "Configure the front door to forward to the installed loopback bind and overwrite XFF.",
  "Return to this dialog and choose Check again.",
] as const;

function workerBootstrapTokenCount(databasePath: string): number {
  const script = `
    import { Database } from "bun:sqlite";
    const databasePath = process.env.ROOST_LOCAL_FIRST_DATABASE;
    if (!databasePath) throw new Error("local-first database path is required");
    const database = new Database(databasePath, { readonly: true, strict: true });
    database.exec("PRAGMA busy_timeout=10000");
    try {
      const row = database.query(
        "SELECT COUNT(*) AS count FROM bootstrap_tokens WHERE kind = 'worker'",
      ).get();
      if (
        !row
        || typeof row.count !== "number"
        || !Number.isSafeInteger(row.count)
        || row.count < 0
      ) {
        throw new Error("worker bootstrap token count was invalid");
      }
      process.stdout.write(String(row.count));
    } finally {
      database.close(false);
    }
  `;
  const output = execFileSync(process.env.ROOST_TEST_BUN ?? "bun", ["-e", script], {
    cwd: REPOSITORY_ROOT,
    encoding: "utf8",
    env: {
      ...process.env,
      ROOST_LOCAL_FIRST_DATABASE: databasePath,
    },
    timeout: 10_000,
  });
  const count = Number(String(output).trim());
  if (!Number.isSafeInteger(count) || count < 0) {
    throw new Error("worker bootstrap token count child returned an invalid value");
  }
  return count;
}

function assertOrderedGuidance(text: string): void {
  let previousOffset = -1;
  for (const instruction of LOCAL_ONLY_GUIDANCE) {
    const offset = text.indexOf(instruction);
    expect(offset, `missing or misordered local-only instruction: ${instruction}`).toBeGreaterThan(previousOffset);
    previousOffset = offset;
  }
}

test("local-first onboarding pairs locally and defers remote machine enrollment", async ({ browser }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop local-first browser contract");
  test.setTimeout(240_000);

  let stack: TerminalTestStack | undefined;
  let unpairedContext: BrowserContext | undefined;
  let pairedContext: BrowserContext | undefined;
  try {
    stack = await startTerminalTestStack({ localFirst: true });
    const localOrigin = new URL(stack.baseUrl);
    expect(localOrigin.protocol).toBe("http:");
    expect(localOrigin.hostname).toBe("127.0.0.1");
    expect(localOrigin.port).not.toBe("");
    expect((await stack.client.authCoordIdentity({})).publicUrl).toBe("");

    unpairedContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await unpairedContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
    const unpairedPage = await unpairedContext.newPage();
    const documentResponse = await unpairedPage.goto(stack.baseUrl, { waitUntil: "domcontentloaded" });
    if (!documentResponse) throw new Error("local coordinator did not return an SPA document response");
    const csp = documentResponse.headers()["content-security-policy"];
    if (!csp) throw new Error("local coordinator SPA response omitted Content-Security-Policy");
    const connectSources = csp.split(";").find((directive) => directive.trimStart().startsWith("connect-src"))
      ?.trim().split(/\s+/).slice(1);
    if (!connectSources) throw new Error("local coordinator CSP omitted connect-src");
    expect(connectSources).not.toContain("http:");
    expect(connectSources).not.toContain("ws:");
    const unpairedWorkersListStatus = await unpairedPage.evaluate(async (path) => {
      const response = await fetch(path, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: "{}",
      });
      return response.status;
    }, WORKERS_LIST_PATH);
    expect(unpairedWorkersListStatus).toBe(401);
    const localCrypto = await unpairedPage.evaluate(async () => ({
      origin: location.origin,
      secure: isSecureContext,
      digestBytes: (await crypto.subtle.digest("SHA-256", new Uint8Array([1]))).byteLength,
    }));
    expect(localCrypto).toEqual({ origin: stack.baseUrl, secure: true, digestBytes: 32 });
    await unpairedContext.close();
    unpairedContext = undefined;

    pairedContext = await browser.newContext({ viewport: { width: 1440, height: 900 } });
    await pairedContext.addInitScript(() => localStorage.setItem("roostSmoke", "1"));
    const pairedPage = await pairedContext.newPage();
    await enrollSmokeBrowser(pairedPage, stack);
    expect(await pairedPage.evaluate(() => location.hash)).toBe("");

    const sessionId = (await spawnSmokeShell(pairedPage, stack.workerFp)).session_id;
    await navigateToSmokeSession(pairedPage, sessionId);
    const marker = uniqueMarker("LOCAL-FIRST-PTY");
    const terminalSlot = pairedPage.getByTestId(`terminal-slot-${sessionId}`);
    const whatsNewDismiss = pairedPage.getByTestId("whats-new-dismiss");
    if (await whatsNewDismiss.count() === 1 && await whatsNewDismiss.isVisible()) {
      await whatsNewDismiss.click();
    }
    await terminalSlot.getByTestId("terminal-display").click();
    await expect(terminalSlot).toHaveAttribute("data-focused", "true");
    await pairedPage.keyboard.type(`printf '%s\\n' ${marker}`);
    await pairedPage.keyboard.press("Enter");
    await expect.poll(() => terminalSlot.textContent(), { timeout: 30_000 }).toContain(marker);

    const workerTokensBeforeGuidance = workerBootstrapTokenCount(stack.coordDbPath);
    expect(workerTokensBeforeGuidance).toBe(1);
    const bootstrapRequests: string[] = [];
    pairedPage.on("request", (request) => {
      if (new URL(request.url()).pathname.endsWith(MINT_BOOTSTRAP_PATH)) bootstrapRequests.push(request.url());
    });

    await pairedPage.getByTestId("workbench-activity-settings").click();
    await expect(pairedPage).toHaveURL(/\/settings\/machines$/);
    await pairedPage.getByTestId("machines-add-btn").click();
    const dialog = pairedPage.getByTestId("machine-deploy-dialog");
    const localOnlyGuide = pairedPage.getByTestId("machine-deploy-local-only");
    await expect(dialog).toBeVisible();
    await expect(dialog).toContainText("Connect another machine");
    await expect(localOnlyGuide).toBeVisible();
    await expect(localOnlyGuide).toContainText(LOCAL_ONLY_GUIDANCE[0]);
    assertOrderedGuidance(await localOnlyGuide.innerText());
    await expect(pairedPage.getByTestId("machine-deploy-generate")).toHaveCount(0);

    const recheck = pairedPage.getByTestId("machine-deploy-recheck");
    await expect(recheck).toHaveText("Check again");
    const refreshedIdentity = pairedPage.waitForResponse(
      (response) => new URL(response.url()).pathname.endsWith(COORD_IDENTITY_PATH),
      { timeout: 30_000 },
    );
    await recheck.click();
    expect((await refreshedIdentity).status()).toBe(200);
    await expect(localOnlyGuide).toContainText(LOCAL_ONLY_GUIDANCE[0]);
    expect(bootstrapRequests).toEqual([]);
    expect(workerBootstrapTokenCount(stack.coordDbPath)).toBe(workerTokensBeforeGuidance);
  } finally {
    if (stack && testInfo.status !== testInfo.expectedStatus) await attachStackLogs(testInfo, stack);
    await pairedContext?.close();
    await unpairedContext?.close();
    await stack?.stop();
  }
});
