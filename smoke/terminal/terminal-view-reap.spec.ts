// Real-flow proof for the one path no other geometry spec crosses: a viewer
// that never comes back. Every case in terminal-multiview-geometry.spec.ts
// pairs its takeViewerOffline with a restore inside the lease, so the
// reaped-then-rejoined path — the phone tab closed for a day, then the same
// session opened on a desktop — is unpinned there. Helpers are shared with that
// file; the SCD policy itself is only ever @roost/shared/viewport's
// minimumTerminalGeometry.

import {
  TERMINAL_VIEW_LEASE_MS,
  TERMINAL_VIEW_SWEEP_MS,
} from "@roost/shared/viewport";
import { test, expect } from "./fixtures.ts";
import { encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  NARROW_TALL_VIEWPORT, WIDE_SHORT_VIEWPORT, forceVisible, waitForPainted,
} from "./terminal-multiview-helpers.ts";
import {
  inputSmokeTerminal, navigateToSmokeSession, spawnPtyFixtureSession, uniqueMarker,
} from "./terminal-helpers.ts";
import {
  attachViewer, expectEffectiveIsConstrainedMinimum, expectPtyGeometry, takeViewerOffline,
  viewerInput, waitForAcceptedViewGeometry, waitForCoordinatorGeometry,
} from "./terminal-multiview-geometry-helpers.ts";

const GEOMETRY_TEST_TIMEOUT_MS = 180_000;
const PTY_CONVERGE_MS = 20_000;
/** The lease is what reaps a viewer that never comes back, plus the sweep ticks
 *  that observe it and publish the emptied membership. Crossing it is the whole
 *  point of this file. */
const REAP_BUDGET_MS = TERMINAL_VIEW_LEASE_MS + 3 * TERMINAL_VIEW_SWEEP_MS;

test("a viewer that never returns stops pinning the PTY and a bigger newcomer repaints at its own size", async ({
  smokePage: widePage,
  secondSmokePage: narrowPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-viewer geometry contract");
  test.setTimeout(GEOMETRY_TEST_TIMEOUT_MS);

  // widePage spawns but does not mount: it is the online window into the
  // coordinator while the only viewer is gone, and the owner of the session's
  // teardown once the viewer that mounted it has been closed.
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(widePage, fixtureWorker);
  await narrowPage.setViewportSize(NARROW_TALL_VIEWPORT);
  await forceVisible(narrowPage, true);
  const narrowViewId = await attachViewer(narrowPage, sessionId);
  const pinned = await waitForCoordinatorGeometry(
    widePage,
    sessionId,
    "the narrow viewer owns the session geometry",
    45_000,
    (snapshot) => snapshot.activeViews === 1
      && snapshot.parkedViews === 0
      && snapshot.inputs.length === 1
      && snapshot.effective !== null,
  );
  const narrowClaim = viewerInput(pinned, narrowViewId);
  expect(pinned.effective).toEqual({ cols: narrowClaim.cols, rows: narrowClaim.rows });
  await expectPtyGeometry(
    narrowPage,
    sessionId,
    pinned.effective!,
    "the narrow viewer's own geometry",
    Date.now() + PTY_CONVERGE_MS,
  );

  // The phone whose tab was closed: its socket dies with no withdraw and the
  // browser is gone, so nothing will ever reclaim this view.
  await takeViewerOffline(narrowPage);
  await narrowPage.close();
  const reaped = await waitForCoordinatorGeometry(
    widePage,
    sessionId,
    "the lease reaped the viewer that never returned",
    REAP_BUDGET_MS,
    (snapshot) => snapshot.activeViews === 0
      && snapshot.parkedViews === 0
      && snapshot.inputs.length === 0,
  );
  expect(reaped.constrained).toBeNull();

  // Only now does the desktop open the same session, at a bigger box.
  await widePage.setViewportSize(WIDE_SHORT_VIEWPORT);
  await forceVisible(widePage, true);
  await navigateToSmokeSession(widePage, sessionId);
  const wideViewId = await attachViewer(widePage, sessionId);
  const rejoined = await waitForCoordinatorGeometry(
    widePage,
    sessionId,
    "the newcomer is the session's only viewer",
    45_000,
    (snapshot) => snapshot.activeViews === 1
      && snapshot.inputs.length === 1
      && snapshot.inputs[0]?.viewId === wideViewId
      && snapshot.effective !== null,
  );
  const wideClaim = viewerInput(rejoined, wideViewId);
  // The departed viewer was the narrower one, so running at the newcomer's own
  // box is an observable change and not the geometry it inherited.
  expect(wideClaim.cols).toBeGreaterThan(narrowClaim.cols);
  const wideGeometry = { cols: wideClaim.cols, rows: wideClaim.rows };
  expect(rejoined.effective).toEqual(wideGeometry);
  expectEffectiveIsConstrainedMinimum(rejoined, "one newcomer viewer");
  await waitForAcceptedViewGeometry(
    widePage,
    sessionId,
    wideGeometry,
    "the newcomer adopted its own geometry",
    30_000,
  );
  await expectPtyGeometry(
    widePage,
    sessionId,
    wideGeometry,
    "the PTY adopted the newcomer's size",
    Date.now() + PTY_CONVERGE_MS,
  );

  // The proof the geometry assertions alone cannot give: the grid is LIVE, not
  // a frozen last frame minted while the departed viewer's claim was minimal.
  const marker = uniqueMarker("REPAINT");
  await inputSmokeTerminal(
    widePage,
    sessionId,
    encodePtyFixtureCommand({ op: "EMIT", text: marker }),
  );
  await waitForPainted(widePage, sessionId, marker);
});
