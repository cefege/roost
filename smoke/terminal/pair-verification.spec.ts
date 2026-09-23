// Real-browser proof that browser pairing needs requester confirmation.
// It uses the ordinary trusted smoke page and a separately created unpaired tab.
// The assertions cover stale protocol rejection, code disclosure, dismissal,
// lost confirmation responses, secret containment, and final worker hydration.

import { Buffer } from "node:buffer";
import type { Page } from "@playwright/test";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairRequestId,
  generatePairRequesterToken,
} from "@roost/shared/pairing";
import { createUnauthenticatedCoordClient } from "../../apps/worker/src/coord-client.ts";
import { expect, test } from "./fixtures.ts";

const WORKERS_LIST_PATH = "/roost.v1.CoordinatorService/WorkersList";
const PAIR_POLL_PATH = "/roost.v1.CoordinatorService/PairPoll";
const PAIR_CONFIRM_PATH = "**/roost.v1.CoordinatorService/PairConfirm";
const REQUESTER_RECORD_KEY = "roost.pairingCeremony.v1";
const APPROVER_RECORD_KEY = "roost.pairApproval.v1";

async function unsignedWorkersListStatus(page: Page): Promise<number> {
  return page.evaluate(async (path) => {
    const response = await fetch(path, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    });
    return response.status;
  }, WORKERS_LIST_PATH);
}

async function generatePublicKeyB64(): Promise<string> {
  const keys = await crypto.subtle.generateKey({ name: "Ed25519" }, true, ["sign", "verify"]);
  return Buffer.from(await crypto.subtle.exportKey("raw", keys.publicKey)).toString("base64");
}

async function expectReloadRequired(operation: Promise<unknown>): Promise<void> {
  await expect(operation).rejects.toMatchObject({
    code: 9,
    rawMessage: "pairing client must reload",
  });
}

async function requesterCeremony(page: Page): Promise<{
  ceremonyVersion: number;
  requesterToken: string;
}> {
  const raw = await page.evaluate((key) => sessionStorage.getItem(key), REQUESTER_RECORD_KEY);
  if (raw === null) throw new Error("requester ceremony record was not saved");
  const record = JSON.parse(raw) as {
    ceremonyVersion?: unknown;
    requesterToken?: unknown;
  };
  if (typeof record.ceremonyVersion !== "number" || typeof record.requesterToken !== "string") {
    throw new Error("requester ceremony record was malformed");
  }
  return {
    ceremonyVersion: record.ceremonyVersion,
    requesterToken: record.requesterToken,
  };
}

test("browser pairing requires a requester-confirmed verification code", async ({
  browser,
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop two-browser pairing contract");
  test.setTimeout(240_000);

  const publicClient = createUnauthenticatedCoordClient(stack.baseUrl);
  const staleLabel = `legacy-pair-${crypto.randomUUID()}`;
  await expectReloadRequired(publicClient.pairCreate({
    ceremonyVersion: 0,
    ephemeralId: generatePairRequestId(),
    requesterToken: generatePairRequesterToken(),
    sshPubkeyB64: await generatePublicKeyB64(),
    label: staleLabel,
  }));
  expect((await stack.client.pairList({})).requests.some((request) => request.label === staleLabel)).toBe(false);

  const requesterContext = await browser.newContext({ viewport: { width: 390, height: 360 } });
  await requesterContext.addInitScript(() => {
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
  });
  const requesterPage = await requesterContext.newPage();
  try {
    await requesterPage.goto(`${stack.baseUrl}/pair`, { waitUntil: "domcontentloaded" });
    const start = requesterPage.getByTestId("onboarding-pair-start-btn");
    await expect(start).toBeVisible({ timeout: 30_000 });
    await start.click();

    const requesterIdSurface = requesterPage.getByTestId("onboarding-pair-ephemeral-id");
    await expect(requesterIdSurface).toBeVisible({ timeout: 30_000 });
    const requesterId = (await requesterIdSurface.locator("code").innerText()).trim();
    expect(requesterId).toMatch(/^[0-9a-f]{32}$/);
    expect(await unsignedWorkersListStatus(requesterPage)).toBe(401);

    await expectReloadRequired(stack.client.pairApprove({
      ceremonyVersion: 0,
      ephemeralId: requesterId,
      verificationCode: "123456",
    }));
    await expect.poll(async () => (
      (await stack.client.pairList({})).requests.some((request) => request.ephemeralId === requesterId)
    )).toBe(true);

    await smokePage.goto(`${stack.baseUrl}/pair`, { waitUntil: "domcontentloaded" });
    const approvalCard = smokePage.locator(
      `[data-testid="pair-request-card"][data-ephemeral-id="${requesterId}"]`,
    );
    await expect(approvalCard).toBeVisible({ timeout: 30_000 });
    await approvalCard.getByTestId("pair-card-approve").click();

    const codeDialog = smokePage.getByTestId("pair-verification-code-dialog");
    const codeSurface = smokePage.getByTestId("pair-verification-code");
    await expect(codeDialog).toBeVisible({ timeout: 30_000 });
    await expect(codeSurface).toHaveCount(1);
    const verificationCode = (await codeSurface.innerText()).replace(/\s/g, "");
    expect(verificationCode).toMatch(/^\d{6}$/);

    const savedRequesterCeremony = await requesterCeremony(requesterPage);
    expect(savedRequesterCeremony.ceremonyVersion).toBe(PAIRING_CEREMONY_VERSION);
    const savedRequesterToken = savedRequesterCeremony.requesterToken;
    for (const url of [requesterPage.url(), smokePage.url()]) {
      expect(url).not.toContain(savedRequesterToken);
      expect(url).not.toContain(verificationCode);
    }
    await expect.poll(() => smokePage.evaluate((key) => sessionStorage.getItem(key), APPROVER_RECORD_KEY))
      .not.toBeNull();

    await smokePage.getByTestId("pair-verification-code-done").click();
    await expect(codeDialog).toHaveCount(0);
    await expect.poll(() => smokePage.evaluate((key) => sessionStorage.getItem(key), APPROVER_RECORD_KEY))
      .toBeNull();

    await expect(requesterPage.getByTestId("onboarding-pair-verification-input"))
      .toBeVisible({ timeout: 30_000 });
    const onboardingRoot = requesterPage.getByTestId("onboarding");
    expect(await onboardingRoot.evaluate((element) =>
      element.scrollHeight > element.clientHeight
    )).toBe(true);
    await onboardingRoot.evaluate((element) => {
      element.scrollTop = 0;
    });
    const onboardingBox = await onboardingRoot.boundingBox();
    if (onboardingBox === null) throw new Error("requester onboarding scroll owner was unavailable");
    await requesterPage.mouse.move(
      onboardingBox.x + onboardingBox.width / 2,
      onboardingBox.y + onboardingBox.height / 2,
    );
    await requesterPage.mouse.wheel(0, 10_000);
    await expect.poll(() => onboardingRoot.evaluate((element) => element.scrollTop))
      .toBeGreaterThan(0);
    await expect.poll(async () => {
      const inputBox = await requesterPage
        .getByTestId("onboarding-pair-verification-input")
        .boundingBox();
      return inputBox !== null && inputBox.y >= 0 && inputBox.y + inputBox.height <= 360;
    }).toBe(true);
    await requesterPage.reload({ waitUntil: "domcontentloaded" });
    await expect(requesterPage.getByTestId("onboarding-pair-verification-input"))
      .toBeVisible({ timeout: 30_000 });
    await expect(requesterPage.getByTestId("folder-list")).toHaveCount(0);
    expect(await unsignedWorkersListStatus(requesterPage)).toBe(401);

    let confirmationCommitted = false;
    let pollsAfterCommittedConfirmation = 0;
    requesterPage.on("request", (request) => {
      if (
        confirmationCommitted
        && new URL(request.url()).pathname.endsWith(PAIR_POLL_PATH)
      ) {
        pollsAfterCommittedConfirmation += 1;
      }
    });

    let confirmationCalls = 0;
    let committedConfirmationStatus: number | undefined;
    await requesterPage.route(PAIR_CONFIRM_PATH, async (route) => {
      confirmationCalls += 1;
      if (confirmationCalls !== 1) {
        await route.continue();
        return;
      }
      const response = await route.fetch();
      committedConfirmationStatus = response.status();
      confirmationCommitted = true;
      await route.abort("failed");
    });

    await requesterPage.getByTestId("onboarding-pair-verification-input").fill(verificationCode);
    await requesterPage.getByTestId("onboarding-pair-confirm").click();
    await expect.poll(() => confirmationCalls).toBe(1);
    await expect.poll(() => committedConfirmationStatus).toBe(200);
    await expect.poll(() => pollsAfterCommittedConfirmation, { timeout: 30_000 }).toBeGreaterThan(0);

    await requesterPage.waitForFunction(
      (workerFp) => !!window.__smoke?.state().workers[workerFp],
      stack.workerFp,
      { timeout: 90_000 },
    );
    expect(confirmationCalls).toBe(1);
    await expect.poll(() => requesterPage.evaluate((key) => sessionStorage.getItem(key), REQUESTER_RECORD_KEY))
      .toBeNull();
    await expect.poll(() => smokePage.evaluate((key) => sessionStorage.getItem(key), APPROVER_RECORD_KEY))
      .toBeNull();
    for (const url of [requesterPage.url(), smokePage.url()]) {
      expect(url).not.toContain(savedRequesterToken);
      expect(url).not.toContain(verificationCode);
    }
  } finally {
    await requesterContext.close();
  }
});
