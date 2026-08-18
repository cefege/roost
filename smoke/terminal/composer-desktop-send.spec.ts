import { test, expect } from "./fixtures.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import {
  spawnSmokeShell,
  navigateToSmokeSession,
  readWorkerBytes,
  inputSmokeTerminal,
  resetTerminalInputCapture,
  readTerminalInputCapture,
} from "./terminal-helpers.ts";
import { attemptPaintedMarker } from "./terminal-paint-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";

test("desktop composer attaches exact files in order without submitting", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop native attachment contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const suffix = crypto.randomUUID().replaceAll("-", "").slice(0, 8);
  const firstName = `attach-first-${suffix}.bin`;
  const secondName = `attach-second-${suffix}.txt`;
  const firstBytes = Buffer.alloc(4 * 1024 * 1024 + 31);
  for (let i = 0; i < firstBytes.length; i++) firstBytes[i] = (i * 17 + suffix.charCodeAt(i % suffix.length)) & 0xff;
  const secondBytes = Buffer.from(`second-${suffix}\nline-two\0tail`, "utf8");

  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const attach = box.getByTestId("chat-attach");
  const input = box.getByTestId("chat-input");
  await expect(attach).toBeVisible();
  await expect(attach).toHaveAttribute("aria-label", "Attach files");
  const [attachBox, inputBox] = await Promise.all([attach.boundingBox(), input.boundingBox()]);
  expect(attachBox).not.toBeNull();
  expect(inputBox).not.toBeNull();
  expect(attachBox!.x + attachBox!.width).toBeLessThanOrEqual(inputBox!.x);

  await input.fill("draft remains untouched");
  await expect(input).toBeFocused();
  const [slotBefore, dockBefore] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotBefore).not.toBeNull();
  expect(dockBefore).not.toBeNull();

  await resetTerminalInputCapture(smokePage);

  const [chooser] = await Promise.all([
    smokePage.waitForEvent("filechooser"),
    attach.click(),
  ]);
  await chooser.setFiles([
    { name: firstName, mimeType: "application/octet-stream", buffer: firstBytes },
    { name: secondName, mimeType: "text/plain", buffer: secondBytes },
  ]);

  await expect(input).toBeFocused();
  await expect(input).toHaveValue("draft remains untouched");
  await expect.poll(async () => {
    const response = await stack.client.listAttachments({ sessionId });
    return [firstName, secondName].every((name) => response.entries.some((entry) => entry.filename === name));
  }, { timeout: 20_000 }).toBe(true);

  const entries = (await stack.client.listAttachments({ sessionId })).entries;
  const firstEntry = entries.find((entry) => entry.filename === firstName);
  const secondEntry = entries.find((entry) => entry.filename === secondName);
  expect(firstEntry).toBeDefined();
  expect(secondEntry).toBeDefined();
  expect(firstEntry!.sizeBytes).toBe(BigInt(firstBytes.length));
  expect(secondEntry!.sizeBytes).toBe(BigInt(secondBytes.length));

  const [storedFirst, storedSecond] = await Promise.all([
    readWorkerBytes(stack.client, stack.workerFp, firstEntry!.absPath),
    readWorkerBytes(stack.client, stack.workerFp, secondEntry!.absPath),
  ]);
  expect(storedFirst).toEqual(new Uint8Array(firstBytes));
  expect(storedSecond).toEqual(new Uint8Array(secondBytes));

  const expectedInput = `${firstEntry!.absPath} ${secondEntry!.absPath} `;
  const expectedBytes = Array.from(new TextEncoder().encode(expectedInput));
  await expect.poll(async () => {
    const capture = await readTerminalInputCapture(smokePage);
    return capture.batches.reduce((total, batch) => total + batch.data.length, 0);
  }).toBe(expectedBytes.length);
  const inputCapture = await readTerminalInputCapture(smokePage);
  expect(inputCapture.droppedBatches).toBe(0);
  expect(inputCapture.batches.every((batch) => batch.sessionId === sessionId)).toBe(true);
  const injected = inputCapture.batches.flatMap((batch) => batch.data);
  expect(injected).toEqual(expectedBytes);
  expect(new TextDecoder().decode(Uint8Array.from(injected))).toBe(expectedInput);
  expect(injected).not.toContain(13);
  expect(injected).not.toContain(10);

  const [slotAfter, dockAfter] = await Promise.all([slot.boundingBox(), dock.boundingBox()]);
  expect(slotAfter).not.toBeNull();
  expect(dockAfter).not.toBeNull();
  for (const key of ["x", "y", "width", "height"] as const) {
    expect(Math.abs(slotAfter![key] - slotBefore![key]), `terminal ${key}`).toBeLessThanOrEqual(1);
    expect(Math.abs(dockAfter![key] - dockBefore![key]), `composer ${key}`).toBeLessThanOrEqual(1);
  }
});

test("desktop composer submits Enter and grows above a stable terminal deck", async ({ smokePage, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop composer keyboard and geometry contract");
  const sessionId = (await spawnSmokeShell(smokePage, stack.workerFp)).session_id;
  await navigateToSmokeSession(smokePage, sessionId);

  const deck = smokePage.getByTestId("terminal-deck");
  const slot = smokePage.getByTestId(`terminal-slot-${sessionId}`);
  const dock = slot.getByTestId("mobile-chat-input");
  const box = dock.getByTestId("chat-box");
  const input = dock.getByTestId("chat-input");
  await expect(deck).toBeVisible();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(input).toBeVisible();
  await box.evaluate(async (el) => {
    await Promise.all(el.getAnimations().map((animation) => animation.finished));
  });

  const readGeometry = () => smokePage.evaluate((id) => {
    const deckEl = document.querySelector('[data-testid="terminal-deck"]');
    const slotEl = document.querySelector(`[data-testid="terminal-slot-${id}"]`);
    const terminalEl = slotEl?.querySelector('[data-testid="terminal-display"]');
    const composerEl = slotEl?.querySelector('[data-testid="mobile-chat-input"]');
    if (
      !(deckEl instanceof HTMLElement)
      || !(slotEl instanceof HTMLElement)
      || !(terminalEl instanceof HTMLElement)
      || !(composerEl instanceof HTMLElement)
    ) return null;

    const slotRect = slotEl.getBoundingClientRect();
    const terminalRect = terminalEl.getBoundingClientRect();
    const composerRect = composerEl.getBoundingClientRect();
    return {
      deckClientHeight: deckEl.clientHeight,
      slotHeight: slotRect.height,
      terminalHeight: terminalRect.height,
      terminalBottom: terminalRect.bottom,
      composerTop: composerRect.top,
      composerHeight: composerRect.height,
    };
  }, sessionId);

  await input.fill("one-row draft");
  await expect(input).toBeFocused();
  const baseline = await readGeometry();
  if (!baseline) throw new Error("desktop composer geometry was unavailable at one row");
  expect(baseline.deckClientHeight).toBeGreaterThan(0);
  expect(baseline.slotHeight).toBeGreaterThan(0);
  expect(baseline.composerHeight).toBeGreaterThan(0);
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim;
    return claim.desired !== null && claim.confirmed?.client_seq === claim.desired.client_seq;
  }, { timeout: 10_000, intervals: [50] }).toBe(true);
  const baselineClaim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim.desired;
  if (!baselineClaim) throw new Error("desktop terminal omitted its baseline viewport claim");

  const growthDraft = [
    "first composer row",
    "second composer row",
    "third composer row",
    "fourth composer row",
    "fifth composer row",
  ].join("\n");
  await input.fill(growthDraft);
  await expect(input).toHaveValue(growthDraft);
  await expect.poll(async () => {
    const geometry = await readGeometry();
    return geometry && geometry.composerHeight > baseline.composerHeight + 1
      ? geometry.terminalBottom - geometry.composerTop
      : Number.POSITIVE_INFINITY;
  }, {
    message: "grown desktop composer must move terminal content above its top",
  }).toBeLessThanOrEqual(0);

  const grown = await readGeometry();
  if (!grown) throw new Error("desktop composer geometry was unavailable after growth");
  expect(grown.composerHeight).toBeGreaterThan(baseline.composerHeight + 1);
  expect(
    Math.abs(grown.deckClientHeight - baseline.deckClientHeight),
    "composer growth must not resize TerminalDeck",
  ).toBeLessThanOrEqual(1);
  expect(
    Math.abs(grown.slotHeight - baseline.slotHeight),
    "composer growth must not resize the terminal slot",
  ).toBeLessThanOrEqual(1);
  expect(grown.terminalBottom, "terminal visual bottom must stay at or above the composer").toBeLessThanOrEqual(
    grown.composerTop,
  );
  expect(
    baseline.terminalHeight - grown.terminalHeight,
    "desktop composer autogrow must resize terminal-display",
  ).toBeGreaterThan(1);
  await expect.poll(async () => {
    const claim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim;
    return claim.desired !== null
      && claim.confirmed?.client_seq === claim.desired.client_seq
      && BigInt(claim.desired.client_seq) > BigInt(baselineClaim.client_seq)
      && claim.desired.cols === baselineClaim.cols
      && claim.desired.rows < baselineClaim.rows;
  }, {
    message: "desktop terminal-display resize must produce one debounced adopted claim",
    timeout: 10_000,
    intervals: [50],
  }).toBe(true);
  const grownClaim = (await readTerminalStreamProbe(smokePage, sessionId)).browser.claim.desired;
  if (!grownClaim) throw new Error("desktop terminal omitted its grown viewport claim");
  expect(BigInt(grownClaim.client_seq) > BigInt(baselineClaim.client_seq)).toBe(true);
  expect(grownClaim.rows).toBeLessThan(baselineClaim.rows);

  const growthOutputMarker = `DESKTOP_GROWTH_LIVE_${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}`;
  await inputSmokeTerminal(smokePage, sessionId, `printf '%s\\n' ${growthOutputMarker}\r`);
  const growthOutputProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: growthOutputMarker });
  expect(growthOutputProof).toMatchObject({ marker: growthOutputMarker, frames: 2 });
  const afterGrowthOutput = await readTerminalStreamProbe(smokePage, sessionId);
  expect(afterGrowthOutput.browser.handler_canonical).toEqual(afterGrowthOutput.browser.dom_reconciled);
  expect(afterGrowthOutput.browser.presentation?.reader_intent).toBe("live");

  const shiftMarker = `DESKTOP_SHIFT_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  const shiftCommand = `printf '%s\\n' ${shiftMarker}`;
  await input.fill(shiftCommand);
  await input.press("Shift+Enter");
  await expect(input).toHaveValue(`${shiftCommand}\n`);

  // A command sent directly after Shift+Enter is a PTY ordering barrier: once
  // it is visible, a mistakenly submitted Shift+Enter command would be visible too.
  const barrier = `DESKTOP_SHIFT_BARRIER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await inputSmokeTerminal(smokePage, sessionId, `printf '%s\\n' ${barrier}\n`);
  const barrierProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: barrier });
  expect(barrierProof).toMatchObject({ marker: barrier, frames: 2 });
  expect((await attemptPaintedMarker(smokePage, sessionId, shiftMarker, 300)).proof).toBeNull();

  const enterMarker = `DESKTOP_ENTER_${crypto.randomUUID().replace(/-/g, "").slice(0, 8)}`;
  await input.fill(`printf '%s\\n' ${enterMarker}`);
  await expect(input).toBeFocused();
  await input.press("Enter");
  const enterProof = await smokePage.evaluate(({ id, marker }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.waitForPaintedMarker(id, marker, 10_000);
  }, { id: sessionId, marker: enterMarker });
  expect(enterProof).toMatchObject({ marker: enterMarker, frames: 2 });
  await expect(input).toHaveValue("");
  await expect(input).toBeFocused();
  await expect(dock).toBeVisible();
  await expect(box).toBeVisible();
  await expect(box).toHaveCount(1);
});
