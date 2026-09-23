// Real-browser proof that browser pairing needs requester confirmation.
// An unpaired tab opened at a protected route sees only the pairing gate; the
// ordinary trusted smoke page approves it. Covers stale protocol rejection,
// secret containment, short-height scrolling, a lost confirmation response
// overtaken by authorization, and automatic retirement of the approver code.

import { Buffer } from "node:buffer";
import {
  PAIRING_CEREMONY_VERSION,
  generatePairRequestId,
  generatePairRequesterToken,
} from "@roost/shared/pairing";
import { createUnauthenticatedCoordClient } from "../../apps/worker/src/coord-client.ts";
import { expect, test } from "./fixtures.ts";
import {
  REQUESTER_RECORD_KEY,
  approveFromTrustedPage,
  approverRecord,
  openUnpairedRequester,
  readRequesterCeremony,
  requestApproval,
  unsignedWorkersListStatus,
  workbenchChromeMounts,
} from "./pair-helpers.ts";

const PAIR_POLL_ROUTE = "**/roost.v1.CoordinatorService/PairPoll";
const PAIR_CONFIRM_ROUTE = "**/roost.v1.CoordinatorService/PairConfirm";
const SHORT_VIEWPORT = { width: 390, height: 360 };

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

  const requester = await openUnpairedRequester(browser, SHORT_VIEWPORT);
  const requesterPage = requester.page;
  try {
    await requesterPage.goto(`${stack.baseUrl}/settings/devices`, { waitUntil: "domcontentloaded" });
    await expect(requesterPage.getByTestId("onboarding")).toBeVisible({ timeout: 30_000 });
    const ceremony = await requestApproval(requesterPage);
    const requesterId = ceremony.ephemeralId;
    await expect(requesterPage.getByTestId("onboarding-pair-ephemeral-id")).toHaveCount(0);
    await expect(requesterPage.getByTestId("onboarding-pair-verification-input")).toHaveCount(0);
    expect(await unsignedWorkersListStatus(requesterPage)).toBe(401);

    await expectReloadRequired(stack.client.pairApprove({
      ceremonyVersion: 0,
      ephemeralId: requesterId,
      verificationCode: "123456",
    }));
    await expect.poll(async () => (
      (await stack.client.pairList({})).requests.some((request) => request.ephemeralId === requesterId)
    )).toBe(true);

    const verificationCode = await approveFromTrustedPage(smokePage, stack.baseUrl, requesterId);
    const codeDialog = smokePage.getByTestId("pair-verification-code-dialog");
    for (const url of [requesterPage.url(), smokePage.url()]) {
      expect(url).not.toContain(ceremony.requesterToken);
      expect(url).not.toContain(verificationCode);
    }
    await expect.poll(() => approverRecord(smokePage)).not.toBeNull();

    // Approval alone is not authority, and the code stays with the approver
    // until the requester proves it.
    const verificationInput = requesterPage.getByTestId("onboarding-pair-verification-input");
    await expect(verificationInput).toBeVisible({ timeout: 30_000 });
    expect(await unsignedWorkersListStatus(requesterPage)).toBe(401);
    await expect(codeDialog).toBeVisible();

    // Short-height reachability: top reachable, then every control reachable by
    // scrolling the page's own scroll owner, with no horizontal overflow.
    await requesterPage.getByTestId("pairing-other-options-toggle").click();
    await expect(requesterPage.getByTestId("onboarding-setup-token-input")).toBeVisible();
    const onboardingRoot = requesterPage.getByTestId("onboarding");
    expect(await onboardingRoot.evaluate((element) => ({
      overflowsVertically: element.scrollHeight > element.clientHeight,
      overflowsHorizontally: element.scrollWidth > element.clientWidth
        || document.documentElement.scrollWidth > window.innerWidth,
    }))).toEqual({ overflowsVertically: true, overflowsHorizontally: false });
    await onboardingRoot.evaluate((element) => {
      element.scrollTop = 0;
    });
    const heading = requesterPage.getByRole("heading", { name: "Pair this browser" });
    const headingBox = await heading.boundingBox();
    expect(headingBox !== null && headingBox.y >= 0).toBe(true);
    const onboardingBox = await onboardingRoot.boundingBox();
    if (onboardingBox === null) throw new Error("requester onboarding scroll owner was unavailable");
    await requesterPage.mouse.move(
      onboardingBox.x + onboardingBox.width / 2,
      onboardingBox.y + onboardingBox.height / 2,
    );
    await requesterPage.mouse.wheel(0, 10_000);
    await expect.poll(() => onboardingRoot.evaluate((element) => element.scrollTop))
      .toBeGreaterThan(0);
    for (const testId of ["onboarding-pair-verification-input", "onboarding-pair-confirm", "onboarding-setup-token-input"]) {
      await requesterPage.getByTestId(testId).scrollIntoViewIfNeeded();
      await expect.poll(async () => {
        const box = await requesterPage.getByTestId(testId).boundingBox();
        return box !== null && box.y >= 0 && box.y + box.height <= SHORT_VIEWPORT.height;
      }).toBe(true);
    }

    await requesterPage.reload({ waitUntil: "domcontentloaded" });
    await expect(verificationInput).toBeVisible({ timeout: 30_000 });
    expect(await unsignedWorkersListStatus(requesterPage)).toBe(401);
    expect(await workbenchChromeMounts(requesterPage)).toEqual([]);

    // Commit the confirmation but lose its response, and hold the requester's
    // recovery poll until authorization has already hydrated the workbench:
    // the requester ceremony must still finish after the gate flips.
    let releaseRecoveryPolls: () => void = () => undefined;
    const recoveryPollsReleased = new Promise<void>((resolve) => {
      releaseRecoveryPolls = resolve;
    });
    let confirmationCommitted = false;
    let heldRecoveryPolls = 0;
    await requesterPage.route(PAIR_POLL_ROUTE, async (route) => {
      if (confirmationCommitted) {
        heldRecoveryPolls += 1;
        await recoveryPollsReleased;
      }
      await route.continue();
    });
    let confirmationCalls = 0;
    let committedConfirmationStatus: number | undefined;
    await requesterPage.route(PAIR_CONFIRM_ROUTE, async (route) => {
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

    await verificationInput.fill(verificationCode);
    await requesterPage.getByTestId("onboarding-pair-confirm").click();
    await expect.poll(() => committedConfirmationStatus).toBe(200);
    await expect.poll(() => heldRecoveryPolls, { timeout: 30_000 }).toBeGreaterThan(0);

    // The approver retires its code without any click once the requester is paired.
    await expect(codeDialog).toHaveCount(0, { timeout: 30_000 });
    await expect(smokePage.getByTestId("pair-verification-code")).toHaveCount(0);
    await expect.poll(() => approverRecord(smokePage)).toBeNull();
    await expect(smokePage.getByText(/New browser paired/)).toHaveCount(1, { timeout: 30_000 });

    await requesterPage.evaluate(() => window.dispatchEvent(new Event("focus")));
    await requesterPage.waitForFunction(
      (workerFp) => !!window.__smoke?.state().workers[workerFp],
      stack.workerFp,
      { timeout: 90_000 },
    );
    releaseRecoveryPolls();
    // The token-bound finalizer clears the record and then redirects to "/";
    // wait for that document so later reads never race the navigation.
    await requesterPage.waitForURL((url) => url.pathname === "/", { timeout: 30_000 });
    await requesterPage.waitForLoadState("domcontentloaded");
    expect(await readRequesterCeremony(requesterPage)).toBeNull();
    await requesterPage.waitForFunction(
      (workerFp) => !!window.__smoke?.state().workers[workerFp],
      stack.workerFp,
      { timeout: 90_000 },
    );
    await expect(requesterPage.locator(".workbench-shell")).toHaveCount(1, { timeout: 30_000 });
    expect(confirmationCalls).toBe(1);
    for (const url of [requesterPage.url(), smokePage.url()]) {
      expect(url).not.toContain(ceremony.requesterToken);
      expect(url).not.toContain(verificationCode);
    }
  } finally {
    await requester.context.close();
  }
});
