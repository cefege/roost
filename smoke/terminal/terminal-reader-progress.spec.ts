// Reader-progress diagnostics prove held marker failures retain browser provenance.
// The test intentionally enters a trusted-wheel reader state before fixture output.
// It verifies diagnostics preserve the caller error and tolerate a closed page.
import { expect, test } from "./fixtures.ts";
import {
  attachFleetFailure,
  captureFleetPresentation,
  disposeFleetReaderTrace,
  installFleetReaderTrace,
} from "./perf-fleet-diagnostics.ts";
import type { FleetPeer } from "./perf-fleet-peer-flood.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import { navigateToSmokeSession, spawnPtyFixtureSession } from "./terminal-helpers.ts";
import { readTerminalStreamProbe } from "./terminal-probe-helpers.ts";
import { sendFixtureCommand } from "./terminal-scale-browser.ts";

test("reader-progress diagnostics preserve held output provenance", async ({ smokePage, browser, stack }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "trusted wheel requires Chromium");
  const worker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(smokePage, worker);
  const peer: FleetPeer = {
    document: { page: smokePage, context: smokePage.context(), initial: true, ownsContext: false },
    session: { id: sessionId, worker, markerPrefix: "reader-progress" },
  };
  await navigateToSmokeSession(smokePage, sessionId);
  const historyPrefix = `READER-PROGRESS-${crypto.randomUUID().replaceAll("-", "").slice(0, 8)}-`;
  await sendFixtureCommand(smokePage, sessionId, encodePtyFixtureCommand({ op: "FLOOD", prefix: historyPrefix, count: 600 }));
  await expect.poll(() => smokePage.evaluate(({ id, marker }) => window.__smoke.markerScan(id, marker).max, {
    id: sessionId,
    marker: historyPrefix,
  }), { timeout: 30_000 }).toBe(600);
  await installFleetReaderTrace([peer]);
  let originalError: unknown = null;
  try {
    const grid = smokePage.getByTestId(`terminal-slot-${sessionId}`).locator(".wterm.cell-grid");
    const box = await grid.boundingBox();
    if (!box) throw new Error("reader-progress terminal grid missing");
    await smokePage.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await smokePage.mouse.wheel(0, -100_000);
    await expect.poll(async () => (await readTerminalStreamProbe(smokePage, sessionId)).browser.presentation, {
      timeout: 10_000,
    }).toMatchObject({ reader_intent: "reading", at_bottom: false });
    const beforeDispatch = await captureFleetPresentation(peer);
    const completionMarker = `READER-PROGRESS-COMPLETE-${crypto.randomUUID().replaceAll("-", "")}`;
    await sendFixtureCommand(smokePage, sessionId, encodePtyFixtureCommand({ op: "EMIT", text: completionMarker }));
    try {
      await smokePage.evaluate(async ({ id, marker }) => window.__smoke.waitForPaintedMarker(id, marker, 1_000), {
        id: sessionId,
        marker: completionMarker,
      });
    } catch (error) {
      originalError = error;
      await attachFleetFailure(peer, completionMarker, beforeDispatch, "fleet-drain-failure.json");
      throw error;
    }
    throw new Error("held-reader marker unexpectedly painted");
  } catch (error) {
    expect(error).toBe(originalError);
    const attachment = testInfo.attachments.find((candidate) => candidate.name === "fleet-drain-failure.json");
    expect(attachment?.body).toBeDefined();
    const body = attachment?.body;
    const parsed = JSON.parse(typeof body === "string" ? body : Buffer.from(body ?? []).toString("utf8"));
    expect(parsed.atFailure.probe.browser.handler_canonical.seq).toBeGreaterThan(parsed.atFailure.probe.browser.dom_reconciled.seq);
    expect(parsed.atFailure.geometry.fromBottom).toBeGreaterThan(0);
    expect(parsed.events.some((event: { type: string; sessionId: string }) => event.type === "wheel" && event.sessionId === sessionId)).toBe(true);
    expect(parsed.eventCapacity).toBe(512);
    expect(parsed.traceComplete).toBe(true);
    expect(parsed.events.some((event: { stage: string; type: string; presentation: { reader_intent: string } | null }) =>
      event.stage === "after_handlers"
      && event.type === "scroll"
      && event.presentation?.reader_intent === "reading",
    )).toBe(true);
    expect(parsed.atFailure.probe.browser.presentation.reader_intent).toBe("reading");
  } finally {
    await disposeFleetReaderTrace([peer]);
  }
  const disposableContext = await browser.newContext();
  const disposablePage = await disposableContext.newPage();
  const closedPeer: FleetPeer = {
    document: { page: disposablePage, context: disposableContext, initial: false, ownsContext: true },
    session: peer.session,
  };
  await disposableContext.close();
  const closedCapture = await captureFleetPresentation(closedPeer);
  expect(closedCapture).toMatchObject({ probe: null, geometry: null });
  expect(closedCapture.diagnosticError).not.toBeNull();
});
