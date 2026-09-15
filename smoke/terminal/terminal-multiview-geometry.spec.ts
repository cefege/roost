// Real-flow proof that the PTY itself runs at the smallest common dimensions
// of the viewers that are actually looking. Two independent browsers with
// crossed axes drive one fixture PTY; every geometry claim is checked against
// the PTY child's own winsize and against the coordinator's published per-view
// inputs. Helpers live in terminal-multiview-geometry-helpers.ts; the SCD
// policy itself is only ever @roost/shared/viewport's minimumTerminalGeometry.

import { setTimeout as delay } from "node:timers/promises";
import {
  TERMINAL_VIEW_HEARTBEAT_MS,
  TERMINAL_VIEW_LEASE_MS,
  TERMINAL_VIEW_PARK_GRACE_MS,
  TERMINAL_VIEW_SWEEP_MS,
} from "@roost/shared/viewport";
import { test, expect } from "./fixtures.ts";
import {
  RESIZED_NARROW_VIEWPORT, WIDE_SHORT_VIEWPORT, forceVisible,
} from "./terminal-multiview-helpers.ts";
import { spawnPtyFixtureSession } from "./terminal-helpers.ts";
import {
  SOCKET_CLOSE_OBSERVED_MS, attachCrossedViewers, attachViewer,
  expectEffectiveIsConstrainedMinimum, expectPtyGeometry, readCoordinatorGeometry,
  readDesiredViewRevision, restoreViewer, takeViewerOffline, viewerInput,
  waitForAcceptedViewGeometry, waitForCoordinatorGeometry,
} from "./terminal-multiview-geometry-helpers.ts";

const GEOMETRY_TEST_TIMEOUT_MS = 180_000;
const PTY_CONVERGE_MS = 20_000;
/** Park grace plus the sweep tick that observes it, plus room for the stream
 *  re-mint and the worker's TIOCSWINSZ. */
const GRACE_RELEASE_BUDGET_MS = TERMINAL_VIEW_PARK_GRACE_MS + TERMINAL_VIEW_SWEEP_MS + 3_000;
/** A parked view used to constrain the PTY until its LEASE expired, and a lease
 *  is refreshed every heartbeat, so the earliest a dead viewer could stop
 *  pinning the session was one lease minus one heartbeat after its socket died.
 *  Every deadline below sits under that: a bound the old behavior could meet
 *  would prove nothing. */
const PRE_FIX_EARLIEST_RELEASE_MS = TERMINAL_VIEW_LEASE_MS - TERMINAL_VIEW_HEARTBEAT_MS;
/** How long the held solo-blip state is resampled before the viewer returns. */
const HOLD_OBSERVATION_MS = 1_500;
const HOLD_SAMPLE_INTERVAL_MS = 250;

test("crossed viewers run the PTY at the per-axis minimum of their own claims", async ({
  smokePage: widePage,
  secondSmokePage: narrowPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-viewer geometry contract");
  test.setTimeout(GEOMETRY_TEST_TIMEOUT_MS);
  expect(widePage.context()).not.toBe(narrowPage.context());

  const session = await attachCrossedViewers(widePage, narrowPage, stack);
  const { wide, narrow, coordinator } = session;

  // Crossed axes: the binding column comes from one browser and the binding row
  // from the other, so neither viewer's own box can be the effective geometry.
  expect(wide.claim.cols).toBeGreaterThan(narrow.claim.cols);
  expect(wide.claim.rows).toBeLessThan(narrow.claim.rows);
  const crossedMinimum = { cols: narrow.claim.cols, rows: wide.claim.rows };
  expect(coordinator.effective).toEqual(crossedMinimum);
  expectEffectiveIsConstrainedMinimum(coordinator, "two live viewers");

  // The coordinator publishing a minimum is not the PTY running at one. Ask the
  // process inside the PTY what the kernel tells it its window is.
  await expectPtyGeometry(
    widePage,
    session.sessionId,
    crossedMinimum,
    "two crossed viewers",
    Date.now() + PTY_CONVERGE_MS,
  );
});

test("a viewer whose socket dies stops pinning the PTY long before its lease", async ({
  smokePage: widePage,
  secondSmokePage: narrowPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-viewer geometry contract");
  test.setTimeout(GEOMETRY_TEST_TIMEOUT_MS);
  expect(GRACE_RELEASE_BUDGET_MS).toBeLessThan(PRE_FIX_EARLIEST_RELEASE_MS);

  const session = await attachCrossedViewers(widePage, narrowPage, stack);
  const { sessionId, wide, narrow } = session;
  const crossedMinimum = { cols: narrow.claim.cols, rows: wide.claim.rows };
  expect(session.coordinator.effective).toEqual(crossedMinimum);
  // The dead viewer is the one clipping the survivor, so re-widening to the
  // survivor's own box is an observable change and not the status quo.
  expect(wide.claim.cols).toBeGreaterThan(crossedMinimum.cols);
  await expectPtyGeometry(
    widePage,
    sessionId,
    crossedMinimum,
    "before the narrow viewer's socket died",
    Date.now() + PTY_CONVERGE_MS,
  );

  const droppedAt = Date.now();
  try {
    // A closed lid or a phone off cellular: the socket dies with no withdraw.
    await takeViewerOffline(narrowPage);
    await waitForCoordinatorGeometry(
      widePage,
      sessionId,
      "coordinator observed the narrow viewer's socket close",
      SOCKET_CLOSE_OBSERVED_MS,
      (snapshot) => snapshot.activeViews === 1
        && snapshot.parkedViews === 1
        && snapshot.inputs.find((input) => input.viewId === narrow.viewId)?.parked === true,
    );
    const parkObservedAt = Date.now();

    const released = await waitForCoordinatorGeometry(
      widePage,
      sessionId,
      "the parked viewer stopped constraining the PTY",
      Math.min(parkObservedAt + GRACE_RELEASE_BUDGET_MS, droppedAt + PRE_FIX_EARLIEST_RELEASE_MS)
        - Date.now(),
      (snapshot) => snapshot.effective !== null
        && snapshot.effective.cols === wide.claim.cols
        && snapshot.effective.rows === wide.claim.rows,
    );
    // The record is still a MEMBER while the session runs at the survivor's own
    // size: the geometry moved because the park grace lapsed, not because the
    // lease reaped the view. Releasing by eviction would show one input here.
    const parkedRecord = viewerInput(released, narrow.viewId);
    expect({ parked: parkedRecord.parked, constrains: parkedRecord.constrains })
      .toEqual({ parked: true, constrains: false });
    expect({
      active: released.activeViews,
      parked: released.parkedViews,
      members: released.inputs.length,
    }).toEqual({ active: 1, parked: 1, members: 2 });
    expectEffectiveIsConstrainedMinimum(released, "one live viewer beside one parked record");

    await waitForAcceptedViewGeometry(
      widePage,
      sessionId,
      wide.claim,
      "the surviving viewer re-widened to its own geometry",
      Math.max(droppedAt + PRE_FIX_EARLIEST_RELEASE_MS - Date.now(), 1),
    );
    await expectPtyGeometry(
      widePage,
      sessionId,
      wide.claim,
      "the PTY re-widened after the dead viewer stopped constraining it",
      droppedAt + PRE_FIX_EARLIEST_RELEASE_MS,
    );
  } finally {
    await restoreViewer(narrowPage);
  }
});

test("a solo viewer's socket blip holds its geometry and its stream", async ({
  smokePage: viewerPage,
  secondSmokePage: observerPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-viewer geometry contract");
  test.setTimeout(GEOMETRY_TEST_TIMEOUT_MS);

  await viewerPage.setViewportSize(WIDE_SHORT_VIEWPORT);
  await forceVisible(viewerPage, true);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(viewerPage, fixtureWorker);
  await attachViewer(viewerPage, sessionId);
  // observerPage never mounts this session. It is a second enrolled browser,
  // and once the sole viewer's socket dies it is the only window left into the
  // coordinator's state.
  const attached = await waitForCoordinatorGeometry(
    observerPage,
    sessionId,
    "the solo viewer owns the session geometry",
    45_000,
    (snapshot) => snapshot.activeViews === 1
      && snapshot.parkedViews === 0
      && snapshot.inputs.length === 1
      && snapshot.effective !== null,
  );
  const held = attached.effective;
  if (!held) throw new Error("the solo viewer settled with no effective geometry");
  expectEffectiveIsConstrainedMinimum(attached, "one live viewer");
  await expectPtyGeometry(
    viewerPage,
    sessionId,
    held,
    "the solo viewer's own geometry",
    Date.now() + PTY_CONVERGE_MS,
  );

  const droppedAt = Date.now();
  try {
    await takeViewerOffline(viewerPage);
    const parked = await waitForCoordinatorGeometry(
      observerPage,
      sessionId,
      "coordinator observed the solo viewer's socket close",
      SOCKET_CLOSE_OBSERVED_MS,
      (snapshot) => snapshot.activeViews === 0 && snapshot.parkedViews === 1,
    );
    expect({ effective: parked.effective, streamId: parked.streamId })
      .toEqual({ effective: held, streamId: attached.streamId });

    // The grace lapses with nobody live. Dropping a parked viewer's geometry
    // the instant it stops constraining would leave this session with no
    // viewers to minimize over, disable its stream, and re-mint one the moment
    // a flapping link came back — the blip is absorbed instead.
    const lapsed = await waitForCoordinatorGeometry(
      observerPage,
      sessionId,
      "the parked record's geometry grace lapsed",
      GRACE_RELEASE_BUDGET_MS,
      (snapshot) => snapshot.inputs.length === 1
        && snapshot.inputs.every((input) => !input.constrains),
    );
    expect(lapsed.constrained).toBeNull();
    expect({
      effective: lapsed.effective,
      streamId: lapsed.streamId,
      unavailable: lapsed.unavailable,
    }).toEqual({ effective: held, streamId: attached.streamId, unavailable: false });

    const holdUntil = Date.now() + HOLD_OBSERVATION_MS;
    while (Date.now() < holdUntil) {
      await delay(HOLD_SAMPLE_INTERVAL_MS);
      const sample = await readCoordinatorGeometry(observerPage, sessionId);
      expect({
        effective: sample.effective,
        streamId: sample.streamId,
        members: sample.inputs.length,
      }).toEqual({ effective: held, streamId: attached.streamId, members: 1 });
    }
  } finally {
    await restoreViewer(viewerPage);
  }

  // Past the lease the record is reaped and the hold no longer applies, so the
  // return has to land inside it for this to be the blip it claims to be.
  const resumed = await waitForCoordinatorGeometry(
    observerPage,
    sessionId,
    "the viewer reclaimed its own session inside the lease",
    droppedAt + TERMINAL_VIEW_LEASE_MS - Date.now(),
    (snapshot) => snapshot.activeViews === 1 && snapshot.parkedViews === 0,
  );
  expect({ effective: resumed.effective, streamId: resumed.streamId })
    .toEqual({ effective: held, streamId: attached.streamId });
  await expectPtyGeometry(
    viewerPage,
    sessionId,
    held,
    "the PTY was never resized by the blip",
    Date.now() + PTY_CONVERGE_MS,
  );
});

test("a viewer resized while offline reclaims its record at the new size", async ({
  smokePage: widePage,
  secondSmokePage: narrowPage,
  stack,
}, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop multi-viewer geometry contract");
  test.setTimeout(GEOMETRY_TEST_TIMEOUT_MS);

  const session = await attachCrossedViewers(widePage, narrowPage, stack);
  const { sessionId, wide, narrow } = session;
  expect(session.coordinator.effective).toEqual({ cols: narrow.claim.cols, rows: wide.claim.rows });

  const droppedAt = Date.now();
  try {
    await takeViewerOffline(narrowPage);
    const parked = await waitForCoordinatorGeometry(
      widePage,
      sessionId,
      "coordinator observed the narrow viewer's socket close",
      SOCKET_CLOSE_OBSERVED_MS,
      (snapshot) => snapshot.parkedViews === 1
        && snapshot.inputs.find((input) => input.viewId === narrow.viewId)?.parked === true,
    );
    // Still a member at its old size: what follows is a reclaim of this record,
    // not a fresh admission after the lease reaped it.
    expect(viewerInput(parked, narrow.viewId).cols).toBe(narrow.claim.cols);

    // The resize must reach the viewer's OWN pending intent before the link
    // returns. Reconnecting first makes the client replay its last accepted
    // intent byte-for-byte, which even a reclaim that cannot adopt a new
    // geometry accepts — the spec would then prove nothing about this case.
    const parkedRevision = await readDesiredViewRevision(narrowPage, sessionId);
    await narrowPage.setViewportSize(RESIZED_NARROW_VIEWPORT);
    await expect.poll(
      () => readDesiredViewRevision(narrowPage, sessionId),
      {
        timeout: 10_000,
        intervals: [50, 100, 250],
        message: "the partitioned viewer measured its resize into a new intent",
      },
    ).not.toBe(parkedRevision);
  } finally {
    await restoreViewer(narrowPage);
  }

  const reclaimed = await waitForCoordinatorGeometry(
    widePage,
    sessionId,
    "the resized viewer rejoined the aggregate at its new size",
    droppedAt + PRE_FIX_EARLIEST_RELEASE_MS - Date.now(),
    (snapshot) => snapshot.activeViews === 2
      && snapshot.parkedViews === 0
      && (snapshot.inputs.find((input) => input.viewId === narrow.viewId)?.cols ?? 0)
        > narrow.claim.cols,
  );
  const rejoined = viewerInput(reclaimed, narrow.viewId);
  expect(rejoined.cols).toBeLessThan(wide.claim.cols);
  const widenedMinimum = { cols: rejoined.cols, rows: wide.claim.rows };
  expect(reclaimed.effective).toEqual(widenedMinimum);
  expectEffectiveIsConstrainedMinimum(reclaimed, "the reclaimed viewer's new size");

  await waitForAcceptedViewGeometry(
    narrowPage,
    sessionId,
    widenedMinimum,
    "the reclaimed viewer adopted the new shared geometry",
    30_000,
  );
  await expectPtyGeometry(
    widePage,
    sessionId,
    widenedMinimum,
    "the PTY adopted the reclaimed viewer's new size",
    Date.now() + PTY_CONVERGE_MS,
  );
});
