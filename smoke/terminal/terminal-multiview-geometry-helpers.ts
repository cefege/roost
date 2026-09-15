// Real-flow helpers for the multi-viewer smallest-common-dimension proofs:
// attach two independent browsers to one fixture PTY, read the coordinator's
// per-view geometry inputs, and ask the PTY child what winsize it actually
// observes. terminal-multiview-geometry.spec.ts is the only caller; the probe
// readers come from terminal-probe-viewer-inputs.ts and the SCD policy itself
// from @roost/shared/viewport, so no copy of it lives in a spec.

import { setTimeout as delay } from "node:timers/promises";
import type { Page } from "@playwright/test";
import type { TerminalGeometry } from "@roost/shared/viewport";
import { expect } from "./fixtures.ts";
import { encodePtyFixtureCommand, PTY_FIXTURE_READY } from "./pty-fixture-protocol.ts";
import type { TerminalTestStack } from "./stack.ts";
import { inputSmokeTerminal, navigateToSmokeSession, spawnPtyFixtureSession } from "./terminal-helpers.ts";
import {
  acceptedGeometry,
  coordinatorTerminalViewState,
  readTerminalStreamProbe,
} from "./terminal-probe-helpers.ts";
import {
  coordinatorConstrainedGeometry,
  coordinatorTerminalViewerInputs,
  type CoordinatorTerminalViewerInput,
} from "./terminal-probe-viewer-inputs.ts";
import {
  NARROW_TALL_VIEWPORT, WIDE_SHORT_VIEWPORT, forceVisible, viewportText, waitForPainted,
} from "./terminal-multiview-helpers.ts";

/** The socket close itself is deterministic (takeViewerOffline closes the tube
 *  before partitioning the context), so this only covers the coordinator's own
 *  close handling. It is a transport bound, not the geometry policy's. */
export const SOCKET_CLOSE_OBSERVED_MS = 5_000;
/** The fixture answers REPORT_SIZE on the next PTY write, so a missing answer
 *  means the replica is still resyncing rather than the size being wrong. */
const PTY_ANSWER_TIMEOUT_MS = 5_000;
const PTY_POLL_INTERVAL_MS = 100;
const COORD_POLL_INTERVALS = [50, 100, 250];

export interface CoordinatorGeometrySnapshot {
  effective: TerminalGeometry | null;
  streamId: string;
  activeViews: number;
  parkedViews: number;
  unavailable: boolean;
  inputs: readonly CoordinatorTerminalViewerInput[];
  /** The minimum over the inputs the coordinator says currently constrain the
   *  session, computed by the shared primitive the coordinator itself uses. */
  constrained: TerminalGeometry | null;
}

export interface AttachedViewer {
  page: Page;
  viewId: string;
  /** This viewer's own claimed geometry, as the coordinator recorded it. */
  claim: TerminalGeometry;
}

export interface CrossedViewerSession {
  sessionId: string;
  wide: AttachedViewer;
  narrow: AttachedViewer;
  coordinator: CoordinatorGeometrySnapshot;
}

export async function readCoordinatorGeometry(
  observer: Page,
  sessionId: string,
): Promise<CoordinatorGeometrySnapshot> {
  const probe = await readTerminalStreamProbe(observer, sessionId);
  const state = coordinatorTerminalViewState(probe);
  if (!state) throw new Error(`coordinator published no terminal view for ${sessionId}`);
  const inputs = coordinatorTerminalViewerInputs(probe);
  if (inputs === null) throw new Error(`coordinator published no viewer inputs for ${sessionId}`);
  return {
    effective: state.effective,
    streamId: state.streamId,
    activeViews: state.activeViews,
    parkedViews: state.parkedViews,
    unavailable: state.unavailable,
    inputs,
    constrained: coordinatorConstrainedGeometry(probe),
  };
}

/** Poll the coordinator through an ONLINE page. The rejected snapshot is the
 *  poll value, so a timeout reports the membership it actually saw. */
export async function waitForCoordinatorGeometry(
  observer: Page,
  sessionId: string,
  label: string,
  timeoutMs: number,
  predicate: (snapshot: CoordinatorGeometrySnapshot) => boolean,
): Promise<CoordinatorGeometrySnapshot> {
  let matched: CoordinatorGeometrySnapshot | null = null;
  await expect.poll(async () => {
    const snapshot = await readCoordinatorGeometry(observer, sessionId);
    if (!predicate(snapshot)) return JSON.stringify(snapshot);
    matched = snapshot;
    return "ready";
  }, {
    timeout: Math.max(timeoutMs, 1),
    intervals: COORD_POLL_INTERVALS,
    message: label,
  }).toBe("ready");
  if (matched === null) throw new Error(`${label} matched without a snapshot`);
  return matched;
}

export function viewerInput(
  snapshot: CoordinatorGeometrySnapshot,
  viewId: string,
): CoordinatorTerminalViewerInput {
  const input = snapshot.inputs.find((candidate) => candidate.viewId === viewId);
  if (!input) {
    throw new Error(`coordinator dropped view ${viewId} from ${JSON.stringify(snapshot.inputs)}`);
  }
  return input;
}

/** The published size IS the minimum over the records the coordinator itself
 *  reports as constraining — asserted against those actual inputs, so no spec
 *  ever carries a second copy of the SCD policy. */
export function expectEffectiveIsConstrainedMinimum(
  snapshot: CoordinatorGeometrySnapshot,
  label: string,
): void {
  expect(snapshot.constrained, `${label}: effective geometry is the minimum of its constraining inputs`)
    .toEqual(snapshot.effective);
}

export async function waitForAcceptedViewGeometry(
  page: Page,
  sessionId: string,
  expected: TerminalGeometry,
  label: string,
  timeoutMs: number,
): Promise<void> {
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    return JSON.stringify({
      geometry: acceptedGeometry(probe.browser.view),
      active: probe.browser.view.active,
    });
  }, {
    timeout: timeoutMs,
    intervals: COORD_POLL_INTERVALS,
    message: label,
  }).toBe(JSON.stringify({ geometry: expected, active: true }));
}

/** Navigate one browser onto a live fixture session and return its view id. */
export async function attachViewer(page: Page, sessionId: string): Promise<string> {
  await navigateToSmokeSession(page, sessionId);
  await waitForPainted(page, sessionId, PTY_FIXTURE_READY);
  let viewId: string | null = null;
  await expect.poll(async () => {
    const probe = await readTerminalStreamProbe(page, sessionId);
    const { view, replica } = probe.browser;
    if (view.status !== "accepted" || !view.active || view.view_id === null) {
      return JSON.stringify(view);
    }
    if (!replica.baseline_ready || replica.expected_stream_id !== view.stream_id) {
      return JSON.stringify(replica);
    }
    viewId = view.view_id;
    return "ready";
  }, {
    timeout: 45_000,
    intervals: COORD_POLL_INTERVALS,
    message: "viewer reached an accepted terminal view",
  }).toBe("ready");
  if (viewId === null) throw new Error("viewer accepted a terminal view without an ID");
  return viewId;
}

/** Two independent browsers on one PTY with deliberately crossed axes: the
 *  wide browser is shorter, the narrow one taller, so neither viewer's own box
 *  can be the effective geometry. Waits only for settled membership — the
 *  geometry relationships themselves are the spec's assertions. */
export async function attachCrossedViewers(
  widePage: Page,
  narrowPage: Page,
  stack: TerminalTestStack,
): Promise<CrossedViewerSession> {
  await Promise.all([
    widePage.setViewportSize(WIDE_SHORT_VIEWPORT),
    narrowPage.setViewportSize(NARROW_TALL_VIEWPORT),
    forceVisible(widePage, true),
    forceVisible(narrowPage, true),
  ]);
  const fixtureWorker = await stack.startPtyFixtureWorker();
  const sessionId = await spawnPtyFixtureSession(widePage, fixtureWorker);
  const wideViewId = await attachViewer(widePage, sessionId);
  const narrowViewId = await attachViewer(narrowPage, sessionId);
  const coordinator = await waitForCoordinatorGeometry(
    widePage,
    sessionId,
    "both viewers joined the session's geometry aggregate",
    45_000,
    (snapshot) => snapshot.activeViews === 2
      && snapshot.parkedViews === 0
      && snapshot.inputs.length === 2
      && snapshot.inputs.every((input) => input.constrains && !input.parked)
      && snapshot.effective !== null
      && !snapshot.unavailable,
  );
  const effective = coordinator.effective;
  if (!effective) throw new Error("the coordinator settled two live viewers with no geometry");
  await Promise.all([
    waitForAcceptedViewGeometry(widePage, sessionId, effective, "wide viewer adopted the shared geometry", 30_000),
    waitForAcceptedViewGeometry(narrowPage, sessionId, effective, "narrow viewer adopted the shared geometry", 30_000),
  ]);
  // A claim is projected to bare cols/rows: specs compare it with toEqual
  // against geometry, and the input record's membership fields are not part of
  // that question.
  const wideClaim = viewerInput(coordinator, wideViewId);
  const narrowClaim = viewerInput(coordinator, narrowViewId);
  return {
    sessionId,
    wide: {
      page: widePage,
      viewId: wideViewId,
      claim: { cols: wideClaim.cols, rows: wideClaim.rows },
    },
    narrow: {
      page: narrowPage,
      viewId: narrowViewId,
      claim: { cols: narrowClaim.cols, rows: narrowClaim.rows },
    },
    coordinator,
  };
}

/** Ask the process inside the PTY for its own winsize. REPORT_SIZE opens a
 *  fresh WriteStream on fd 1, so the answer is the kernel's TIOCGWINSZ for the
 *  PTY slave — what `stty size` prints — not anything the browser believes. */
export async function expectPtyGeometry(
  page: Page,
  sessionId: string,
  expected: TerminalGeometry,
  label: string,
  deadlineMs: number,
): Promise<void> {
  let observed: TerminalGeometry | null = null;
  while (Date.now() < deadlineMs) {
    const nonce = `SZ-${crypto.randomUUID().replaceAll("-", "").slice(0, 8).toUpperCase()}`;
    await inputSmokeTerminal(page, sessionId, encodePtyFixtureCommand({ op: "REPORT_SIZE", nonce }));
    // A resize the coordinator has published can still be in flight to the
    // worker, so a stale answer re-probes rather than failing.
    const answer = await readReportedPtySize(
      page,
      sessionId,
      nonce,
      Math.min(Date.now() + PTY_ANSWER_TIMEOUT_MS, deadlineMs),
    );
    if (answer === null) continue;
    observed = answer;
    if (answer.cols === expected.cols && answer.rows === expected.rows) return;
  }
  expect(observed, `${label}: the PTY child's own winsize`).toEqual(expected);
}

/** Kill this viewer's Sync socket the way a closed lid or a dropped cellular
 *  link does: the transport disappears with no withdraw command, and the
 *  viewer cannot return until the link does.
 *
 *  The close is issued BEFORE the context goes offline on purpose. Chromium's
 *  offline emulation black-holes an established socket instead of delivering
 *  its FIN: measured against this stack, a coordinator on the far side of it
 *  still held a solo viewer's view live after 60s, and a second viewer's took
 *  3-8s of failed writes to notice. Closing first reproduces exactly what a
 *  dead link leaves the coordinator — a socket gone, its views never
 *  withdrawn — and setOffline then holds the viewer away as the outage would.
 */
export async function takeViewerOffline(page: Page): Promise<void> {
  await page.evaluate(() => window.__smoke.pauseSyncTransport());
  await page.context().setOffline(true);
}

/** Restore the link and wake the redial ladder, whose capped backoff would
 *  otherwise decide when this viewer returns. */
export async function restoreViewer(page: Page): Promise<void> {
  await page.context().setOffline(false);
  await page.evaluate(() => window.__smoke.resumeSyncTransport());
}

/** The viewer's OWN pending view revision, read from browser state alone. The
 *  layered probe needs a coordinator RPC, and a partitioned page has none; this
 *  is how a spec knows a resize taken while offline has entered the intent the
 *  viewer will claim with when its link returns. */
export function readDesiredViewRevision(page: Page, sessionId: string): Promise<string | null> {
  return page.evaluate((id) => window.__smoke.terminalBrowserSnapshot(id).view.revision, sessionId);
}

async function readReportedPtySize(
  page: Page,
  sessionId: string,
  nonce: string,
  deadlineMs: number,
): Promise<TerminalGeometry | null> {
  const pattern = new RegExp(`SIZE:${nonce}:(\\d+)x(\\d+)`);
  do {
    const match = pattern.exec(await viewportText(page, sessionId));
    if (match) return { cols: Number(match[1]), rows: Number(match[2]) };
    await delay(PTY_POLL_INTERVAL_MS);
  } while (Date.now() < deadlineMs);
  return null;
}
