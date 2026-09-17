// Real-flow proof that the direct-to-worker terminal transport arms from the
// COORDINATOR's own origin: one page loaded off the coordinator's door finds
// the worker running on the same machine, retargets that worker's session onto
// its loopback socket, and both cell frames and keystrokes travel it.
// Owns a private stack (no injected link delay) because it drives the
// coordinator origin against a fixture worker's reserved local UI port.

import { test, expect } from "./fixtures.ts";
import { PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import { LOCAL_TERMINAL_PROCESS_EPOCH } from "../../apps/web/src/store/terminal-stream-transport.ts";
import { startTerminalTestStack } from "./stack.ts";
import { navigateToSmokeSession, spawnPtyFixtureSession } from "./terminal-helpers.ts";
import {
  type EnrolledPage,
  armPaintedMarkerEpoch,
  attachStackLogs,
  emitMarkerFromPage,
  openEnrolledPage,
  readArmedPaintedMarkerEpoch,
  readLocalTransportReading,
} from "./terminal-local-fast-path-helpers.ts";
import { forceVisible, waitForPainted } from "./terminal-multiview-helpers.ts";

test("coordinator-served page takes the direct path to a worker on its own machine @serial", async ({
  browser,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop local terminal fast path");
  test.setTimeout(300_000);

  const stack = await startTerminalTestStack();
  let dashboard: EnrolledPage | undefined;
  try {
    const fixtureWorker = await stack.startPtyFixtureWorker({});
    // The harness assigns every worker a reserved loopback port, so the page
    // needs the operator override to find this door instead of the 4104
    // default. This is the only stack-specific input to the whole flow.
    const doorOrigin = new URL(stack.localUiUrl(fixtureWorker.workerFp)).origin;
    dashboard = await openEnrolledPage(browser, stack, stack.baseUrl, {
      localWorkerOrigin: doorOrigin,
    });
    const page = dashboard.page;
    // This document came off the coordinator, not the worker's door: everything
    // below is therefore discovery plus a cross-origin dial, not a same-origin
    // page talking to its own server.
    expect(page.url()).toContain(new URL(stack.baseUrl).host);
    expect(doorOrigin).not.toBe(new URL(stack.baseUrl).origin);
    await forceVisible(page, true);

    const sessionId = await spawnPtyFixtureSession(page, fixtureWorker);
    await navigateToSmokeSession(page, sessionId);
    await waitForPainted(page, sessionId, PTY_FIXTURE_READY);

    // The view decision and every accepted cell frame pass the SAME generation
    // fence, so a local process epoch on the accepted frame means these cells
    // arrived on the worker's socket rather than the coordinator's Sync tube.
    await expect.poll(
      () => readLocalTransportReading(page, sessionId),
      { timeout: 60_000, intervals: [100, 250, 500] },
    ).toMatchObject({
      acceptedFrameEpoch: LOCAL_TERMINAL_PROCESS_EPOCH,
      viewStatus: "accepted",
      baselineReady: true,
    });
    // The marker a user actually reads, now on a coordinator-origin page.
    await expect(page.getByTestId(`tab-${sessionId}`))
      .toHaveAttribute("data-local-transport", "true");

    // Keystrokes travel the same socket: arm the product's own paint proof
    // before the marker can exist, then type it from this page.
    const marker = `LXO-${crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase()}`;
    await armPaintedMarkerEpoch(page, sessionId, marker);
    const sentAt = await emitMarkerFromPage(page, sessionId, marker);
    expect(await readArmedPaintedMarkerEpoch(page)).toBeGreaterThanOrEqual(sentAt);
  } finally {
    if (testInfo.status !== testInfo.expectedStatus) await attachStackLogs(testInfo, stack);
    await dashboard?.close();
    await stack.stop();
  }
});
