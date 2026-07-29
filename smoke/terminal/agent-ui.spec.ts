// Regression cover for the agent transcript's composer.
//
// The bug this exists for: the composer was a Material md-outlined-text-field
// whose real <textarea> lives in a shadow root, so CellTerminal's document-level
// mousedown guard (which decides with `target.closest(FOCUS_OWNERS)` and sees
// only the retargeted HOST element) preventDefault()ed the click and focus never
// arrived — no caret, Send never enabled, Enter dead. Every assertion below is
// therefore about REAL focus and REAL keystrokes: `fill()` would have passed
// against the broken build, which is exactly how it shipped.

import { test, expect } from "./fixtures.ts";
import type { Page } from "@playwright/test";

type SmokeWindow = Window & {
  __smoke: {
    spawnAgent(worker: string, folder: string): Promise<{ session_id: string }>;
    injectAgentEntries(sessionId: string, entries: unknown[]): void;
  };
};

async function openAgent(page: Page, workerFp: string, baseUrl: string): Promise<string> {
  const sessionId = await page.evaluate(async (fp) => {
    const smoke = (window as unknown as SmokeWindow).__smoke;
    return (await smoke.spawnAgent(fp, "/tmp")).session_id;
  }, workerFp);
  await page.goto(`${baseUrl}/s/${sessionId}`);
  await expect(page.getByTestId("transcript-deck")).toBeVisible();
  return sessionId;
}

/** The composer only counts as focused if the browser says so — a stolen-focus
 *  guard leaves activeElement on <body> while the caret looks plausible. */
async function expectComposerFocused(page: Page): Promise<void> {
  await expect
    .poll(() =>
      page.evaluate(() => (document.activeElement as HTMLElement | null)?.getAttribute("data-testid") ?? null),
    )
    .toBe("agent-composer-input");
}

test("composer takes focus and sends", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  await openAgent(smokePage, stack.workerFp, stack.baseUrl);

  await smokePage.getByTestId("agent-composer-input").click();
  await expectComposerFocused(smokePage);

  await smokePage.keyboard.type("ping");
  await expect(smokePage.getByTestId("agent-composer-input")).toHaveValue("ping");
  await expect(smokePage.getByTestId("agent-composer-send")).toBeEnabled();

  await smokePage.getByTestId("agent-composer-send").click();
  // The worker appends the user's own entry to the ring and re-emits it, so this
  // lands without waiting on the model. Never assert on model output.
  await expect(smokePage.getByTestId("transcript-deck")).toContainText("ping", { timeout: 30_000 });
  await expect(smokePage.getByTestId("agent-composer-input")).toHaveValue("");
  await expect(smokePage.getByText(/Send failed/)).toHaveCount(0);
  await expect(smokePage.getByTestId("error-boundary")).toHaveCount(0);
});

test("Enter sends and Shift+Enter newlines", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop keyboard contract");
  await openAgent(smokePage, stack.workerFp, stack.baseUrl);

  const input = smokePage.getByTestId("agent-composer-input");
  await input.click();
  await expectComposerFocused(smokePage);

  await smokePage.keyboard.type("line1");
  await smokePage.keyboard.press("Shift+Enter");
  await smokePage.keyboard.type("line2");
  await expect(input).toHaveValue("line1\nline2");

  await smokePage.keyboard.press("Enter");
  await expect(input).toHaveValue("");
  await expect(smokePage.getByTestId("transcript-deck")).toContainText("line2", { timeout: 30_000 });
  await expect(smokePage.getByTestId("error-boundary")).toHaveCount(0);
});

test("prompt card answers over SessionsAgentRespond", async ({ smokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "chromium-desktop", "desktop click contract");
  const sessionId = await openAgent(smokePage, stack.workerFp, stack.baseUrl);

  // A real tool approval is too slow to provoke; the card's contract is that a
  // click leaves the browser as SessionsAgentRespond, which is what we assert.
  await smokePage.evaluate((sid) => {
    (window as unknown as SmokeWindow).__smoke.injectAgentEntries(sid, [
      {
        kind: "prompt",
        seq: 1,
        ts: Date.now(),
        prompt_id: "ui-req-7",
        prompt_kind: "approval",
        title: "Allow tool: bash\n$ ls",
        options: ["Approve", "Deny"],
        allow_free_text: false,
        state: "pending",
        answer: "",
      },
    ]);
  }, sessionId);

  await expect(smokePage.getByTestId("agent-entry-prompt")).toBeVisible();
  const req = smokePage.waitForRequest((r) => r.url().includes("/SessionsAgentRespond"));
  await smokePage.getByTestId("agent-prompt-option-0").click();
  await req;
  await expect(smokePage.getByTestId("error-boundary")).toHaveCount(0);
});

test("mobile tap opens the composer keyboard path", async ({ mobileSmokePage, stack }, testInfo) => {
  test.skip(testInfo.project.name !== "webkit-iphone", "iOS focus + zoom-floor contract");
  await openAgent(mobileSmokePage, stack.workerFp, stack.baseUrl);

  await mobileSmokePage.getByTestId("agent-composer-input").tap();
  await expectComposerFocused(mobileSmokePage);
  // 16px floor: iOS Safari zooms the viewport for anything smaller, which yanks
  // the whole transcript out from under the user.
  const fontSize = await mobileSmokePage
    .getByTestId("agent-composer-input")
    .evaluate((el) => getComputedStyle(el).fontSize);
  expect(fontSize).toBe("16px");
  await expect(mobileSmokePage.getByTestId("error-boundary")).toHaveCount(0);
});
