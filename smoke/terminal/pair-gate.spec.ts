// Real-browser proof of the unpaired pairing gate and approver cancellation.
// An unpaired browser at any protected route sees one pairing page at phone,
// tablet, and desktop sizes with the workbench never mounted; an approver who
// closes the code window cancels the request on the server.

import { expect, test } from "./fixtures.ts";
import {
  approveFromTrustedPage,
  approverRecord,
  openUnpairedRequester,
  requestApproval,
  requesterServerStatus,
  workbenchChromeMounts,
} from "./pair-helpers.ts";

const GATE_VIEWPORTS = [
  { name: "phone", width: 390, height: 844, route: "/" },
  { name: "tablet", width: 768, height: 1024, route: "/settings/devices" },
  { name: "desktop", width: 1440, height: 900, route: "/search" },
] as const;

test("unpaired browser sees only the pairing gate at every size", async ({
  browser,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop multi-viewport gate contract");
  test.setTimeout(180_000);

  for (const viewport of GATE_VIEWPORTS) {
    const requester = await openUnpairedRequester(browser, viewport);
    const page = requester.page;
    try {
      await page.goto(`${stack.baseUrl}${viewport.route}`, { waitUntil: "domcontentloaded" });
      const gate = page.getByTestId("onboarding");
      await expect(gate, viewport.name).toBeVisible({ timeout: 30_000 });
      await expect(page.getByRole("heading", { name: "Pair this browser" })).toBeVisible();

      const primary = page.getByTestId("onboarding-pair-start-btn");
      await expect(primary).toBeVisible();
      await expect(primary).toBeInViewport();
      expect(await page.evaluate(() => ({
        documentOverflow: document.documentElement.scrollWidth > window.innerWidth,
      })), viewport.name).toEqual({ documentOverflow: false });
      expect(await gate.evaluate((element) => element.scrollWidth > element.clientWidth)).toBe(false);

      const optionsToggle = page.getByTestId("pairing-other-options-toggle");
      await expect(optionsToggle).toHaveAttribute("aria-expanded", "false");
      await expect(page.getByTestId("onboarding-setup-token-input")).toHaveCount(0);
      const [primaryBox, toggleBox] = await Promise.all([primary.boundingBox(), optionsToggle.boundingBox()]);
      expect(primaryBox !== null && toggleBox !== null && primaryBox.y < toggleBox.y).toBe(true);
      await optionsToggle.click();
      await expect(optionsToggle).toHaveAttribute("aria-expanded", "true");
      await expect(page.getByTestId("onboarding-setup-token-input")).toBeVisible();
      await expect(page.getByTestId("onboarding-setup-token-input")).not.toBeFocused();

      expect(await workbenchChromeMounts(page), viewport.name).toEqual([]);
    } finally {
      await requester.context.close();
    }
  }
});

test("closing the approver code window cancels the request on the server", async ({
  browser,
  smokePage,
  stack,
}, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop two-browser pairing contract");
  test.setTimeout(180_000);

  const requester = await openUnpairedRequester(browser, { width: 390, height: 844 });
  const requesterPage = requester.page;
  try {
    await requesterPage.goto(`${stack.baseUrl}/`, { waitUntil: "domcontentloaded" });
    const ceremony = await requestApproval(requesterPage);
    await approveFromTrustedPage(smokePage, stack.baseUrl, ceremony.ephemeralId);
    await expect(requesterPage.getByTestId("onboarding-pair-verification-input"))
      .toBeVisible({ timeout: 30_000 });
    expect(await requesterServerStatus(stack.baseUrl, ceremony)).toBe("verification_required");

    await smokePage.getByTestId("pair-verification-code-cancel").click();
    await expect(smokePage.getByTestId("pair-verification-code-dialog")).toHaveCount(0, {
      timeout: 30_000,
    });
    await expect.poll(() => approverRecord(smokePage)).toBeNull();
    await expect.poll(() => requesterServerStatus(stack.baseUrl, ceremony), { timeout: 30_000 })
      .toBe("denied");
    await expect(requesterPage.getByTestId("onboarding-pair-verification-input"))
      .toHaveCount(0, { timeout: 30_000 });
    await expect(smokePage.getByText(/New browser paired/)).toHaveCount(0);
    expect(await workbenchChromeMounts(requesterPage)).toEqual([]);
  } finally {
    await requester.context.close();
  }
});
