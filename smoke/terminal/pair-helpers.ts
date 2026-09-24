// Shared real-browser pairing helpers for the pairing smoke specs.
// Opens isolated unpaired requester contexts that record every workbench-chrome
// mount from before first paint, and reads requester/approver ceremony state
// from tab storage so no spec depends on the request ID being visible.

import type { Browser, BrowserContext, Page } from "@playwright/test";
import { createUnauthenticatedCoordClient } from "../../apps/worker/src/coord-client.ts";
import { PAIRING_CEREMONY_VERSION } from "@roost/protocol/pairing";
import { expect } from "./fixtures.ts";

export const REQUESTER_RECORD_KEY = "roost.pairingCeremony.v1";
export const APPROVER_RECORD_KEY = "roost.pairApproval.v1";
const WORKERS_LIST_PATH = "/roost.v1.CoordinatorService/WorkersList";

/** Anything the authorized workbench mounts; none may exist before authority. */
const WORKBENCH_CHROME_SELECTOR = [
  ".workbench-shell",
  "[data-testid=\"sidebar-desktop\"]",
  "[data-testid=\"folder-list\"]",
  "[data-testid=\"terminal-deck\"]",
  "[data-testid^=\"workbench-activity-\"]",
].join(", ");

declare global {
  interface Window {
    /** Selectors of workbench chrome observed in this document, if any. */
    __roostChromeMounts?: string[];
  }
}

export interface RequesterCeremony {
  ceremonyVersion: number;
  ephemeralId: string;
  requesterToken: string;
}

export interface UnpairedRequester {
  context: BrowserContext;
  page: Page;
}

export async function openUnpairedRequester(
  browser: Browser,
  viewport: { width: number; height: number },
  options: { tvMode?: boolean } = {},
): Promise<UnpairedRequester> {
  const context = await browser.newContext({ viewport, hasTouch: false });
  await context.addInitScript(({ selector, tvMode }) => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    if (tvMode) localStorage.setItem("roost.tvMode", "on");
    const mounts: string[] = [];
    window.__roostChromeMounts = mounts;
    const recordChrome = (): void => {
      for (const element of document.querySelectorAll(selector)) {
        const label = element.getAttribute("data-testid") ?? element.className.toString();
        if (!mounts.includes(label)) mounts.push(label);
      }
    };
    new MutationObserver(recordChrome).observe(document, { childList: true, subtree: true });
  }, { selector: WORKBENCH_CHROME_SELECTOR, tvMode: options.tvMode === true });
  const page = await context.newPage();
  return { context, page };
}

export async function workbenchChromeMounts(page: Page): Promise<string[]> {
  return page.evaluate(() => window.__roostChromeMounts ?? []);
}

export async function unsignedWorkersListStatus(page: Page): Promise<number> {
  return page.evaluate(async (path) => {
    const response = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    return response.status;
  }, WORKERS_LIST_PATH);
}

export async function readRequesterCeremony(page: Page): Promise<RequesterCeremony | null> {
  const raw = await page.evaluate((key) => sessionStorage.getItem(key), REQUESTER_RECORD_KEY);
  if (raw === null) return null;
  const record = JSON.parse(raw) as Partial<RequesterCeremony>;
  if (
    typeof record.ceremonyVersion !== "number"
    || typeof record.ephemeralId !== "string"
    || typeof record.requesterToken !== "string"
  ) {
    throw new Error("requester ceremony record was malformed");
  }
  return {
    ceremonyVersion: record.ceremonyVersion,
    ephemeralId: record.ephemeralId,
    requesterToken: record.requesterToken,
  };
}

/** Clicks the one primary action and returns the saved tab-scoped ceremony. */
export async function requestApproval(page: Page): Promise<RequesterCeremony> {
  const start = page.getByTestId("onboarding-pair-start-btn");
  await expect(start).toBeVisible({ timeout: 30_000 });
  await start.click();
  await expect.poll(() => readRequesterCeremony(page), { timeout: 30_000 }).not.toBeNull();
  const ceremony = await readRequesterCeremony(page);
  if (ceremony === null) throw new Error("requester ceremony record was not saved");
  expect(ceremony.ceremonyVersion).toBe(PAIRING_CEREMONY_VERSION);
  expect(ceremony.ephemeralId).toMatch(/^[0-9a-f]{32}$/);
  return ceremony;
}

/** Approves one exact request from the trusted page and returns its code. */
export async function approveFromTrustedPage(
  trustedPage: Page,
  baseUrl: string,
  ephemeralId: string,
): Promise<string> {
  await trustedPage.goto(`${baseUrl}/pair`, { waitUntil: "domcontentloaded" });
  const approvalCard = trustedPage.locator(
    `[data-testid="pair-request-card"][data-ephemeral-id="${ephemeralId}"]`,
  );
  await expect(approvalCard).toBeVisible({ timeout: 30_000 });
  await approvalCard.getByTestId("pair-card-approve").click();
  await expect(trustedPage.getByTestId("pair-verification-code-dialog")).toBeVisible({
    timeout: 30_000,
  });
  const codeSurface = trustedPage.getByTestId("pair-verification-code");
  await expect(codeSurface).toHaveCount(1);
  const verificationCode = (await codeSurface.innerText()).replace(/\s/g, "");
  expect(verificationCode).toMatch(/^\d{6}$/);
  return verificationCode;
}

/** The requester's own token-bound view of the server row. */
export async function requesterServerStatus(
  baseUrl: string,
  ceremony: RequesterCeremony,
): Promise<string> {
  const response = await createUnauthenticatedCoordClient(baseUrl).pairPoll({
    ceremonyVersion: ceremony.ceremonyVersion,
    ephemeralId: ceremony.ephemeralId,
    requesterToken: ceremony.requesterToken,
  });
  return response.status;
}

export async function approverRecord(page: Page): Promise<string | null> {
  return page.evaluate((key) => sessionStorage.getItem(key), APPROVER_RECORD_KEY);
}
