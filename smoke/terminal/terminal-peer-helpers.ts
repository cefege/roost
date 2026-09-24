// Direct terminal peer smoke helpers own route observation and trusted-key proof.
// The peer specs use real coordinator, worker, keeper, and PTY paths through
// the existing terminal stack. They observe elected routes instead of inferring
// a carrier from an epoch sentinel or a mocked forwarding callback.

import type { Browser, Page } from "@playwright/test";
import type { TerminalBrowserStreamSnapshot } from "../../apps/web/src/lib/terminalDiagSnapshot.ts";
import type { TerminalTimingResult } from "../../apps/web/src/lib/smokeHarness.ts";
import { enrollSmokeBrowser, expect } from "./fixtures.ts";
import {
  type EnrolledPage,
  openEnrolledPage,
} from "./terminal-local-fast-path-helpers.ts";
import { PTY_FIXTURE_READY, encodePtyFixtureCommand } from "./pty-fixture-protocol.ts";
import {
  installDisabledLoopbackProbe,
  installRtcUnavailable,
  installRtcAnswerHold,
} from "./stack-browser-faults.ts";
import type { TerminalTestStack, TerminalTestWorker } from "./stack.ts";
import { navigateToSmokeSession, spawnPtyFixtureSession, switchToSmokeSession } from "./terminal-helpers.ts";
import { expectMarkersOnce, forceVisible, waitForPainted } from "./terminal-multiview-helpers.ts";
import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";

const PEER_ROUTE_TIMEOUT_MS = Number(process.env.ROOST_PEER_ROUTE_TIMEOUT_MS ?? 60_000);
const PEER_ROUTE_INTERVALS_MS = [100, 250, 500] as const;

export type TerminalPeerRouteKind = "sync" | "loopback" | "webrtc";
export type DirectTerminalPeerRouteKind = Exclude<TerminalPeerRouteKind, "sync">;

export interface PeerRouteReading {
  activeKind: TerminalPeerRouteKind | null;
  activeWorkerEpoch: string | null;
  activePeerId: string | null;
  activeProbeAgeMs: number | null;
  activeRttMs: number | null;
  activeCandidateType: "host" | "srflx" | "prflx" | "none";
  activeWorkerControlRttMs: number | null;
  activeBufferedBytes: number | null;
  candidateKind: TerminalPeerRouteKind | null;
  candidateWorkerEpoch: string | null;
  candidatePeerId: string | null;
  peerPhase: string | null;
  candidateCandidateType: "host" | "srflx" | "prflx" | "none";
  pendingInputCount: number;
  fallbackReason: string | null;
  failureDetail: string | null;
  inputPhase: string | null;
  proofKind: TerminalPeerRouteKind | null;
  proofSocketId: string | null;
  proofSocketGeneration: number | null;
  proofWorkerEpoch: string | null;
  baselineReady: boolean;
  syncReady: boolean;
}

export interface PeerSmokePageOptions {
  origin?: string;
  localWorkerOrigin?: string;
  rtcUnavailable?: boolean;
  holdRtcAnswer?: boolean;
}
export interface TrustedPeerKeyResult {
  nonce: string;
  marker: string;
  timing: TerminalTimingResult;
}

/** Opens a normal enrolled page, then pins it foregrounded so peer demand is live. */
export async function openPeerSmokePage(
  browser: Browser,
  stack: TerminalTestStack,
  options: PeerSmokePageOptions = {},
): Promise<EnrolledPage> {
  const origin = options.origin ?? stack.baseUrl;
  if (options.rtcUnavailable || options.holdRtcAnswer || (stack.disableLoopbackProbe && !options.localWorkerOrigin)) {
    return openPeerSmokePageWithFaults(
      browser,
      stack,
      origin,
      options.localWorkerOrigin,
      options.rtcUnavailable === true,
      options.holdRtcAnswer === true,
    );
  }
  const enrolled = await openEnrolledPage(
    browser,
    stack,
    origin,
    { localWorkerOrigin: options.localWorkerOrigin },
  );
  await forceVisible(enrolled.page, true);
  return enrolled;
}

async function openPeerSmokePageWithFaults(
  browser: Browser,
  stack: TerminalTestStack,
  origin: string,
  localWorkerOrigin: string | undefined,
  rtcUnavailable: boolean,
  holdRtcAnswer: boolean,
): Promise<EnrolledPage> {
  const context = await browser.newContext();
  if (stack.disableLoopbackProbe && !localWorkerOrigin) {
    await installDisabledLoopbackProbe(context);
  }
  if (rtcUnavailable) await installRtcUnavailable(context);
  if (holdRtcAnswer) await installRtcAnswerHold(context);
  await context.addInitScript((localOrigin: string | undefined) => {
    if (localOrigin) localStorage.setItem("roost.localWorkerOrigin", localOrigin);
    localStorage.setItem("roostSmoke", "1");
    localStorage.setItem("roost.whatsNew.lastSeenVersion", "2.0.0");
    localStorage.setItem("roost.sidebarView", "folders");
    localStorage.setItem("roost.sidebarCollapsed", "0");
  }, localWorkerOrigin);
  const page = await context.newPage();
  try {
    await enrollSmokeBrowser(page, { ...stack, baseUrl: origin }, stack.client);
  } catch (error) {
    await context.close();
    throw error;
  }
  await forceVisible(page, true);
  return {
    page,
    context,
    origin,
    close: async () => {
      await page.evaluate(async () => {
        window.__smoke?.forceVisible(false);
        await window.__smoke?.cleanupCreated();
      }).catch(() => undefined);
      await context.close();
    },
  };
}

/** Starts an ordinary fixture-backed PTY and proves its initial row reached paint. */
export async function createPeerFixtureSession(
  page: Page,
  worker: TerminalTestWorker,
): Promise<string> {
  const sessionId = await spawnPtyFixtureSession(page, worker);
  await navigateToSmokeSession(page, sessionId);
  await waitForPainted(page, sessionId, PTY_FIXTURE_READY);
  return sessionId;
}
/** Adds a PTY without replacing the browser document that owns its worker peer. */
export async function createPeerFixtureSessionInDocument(
  page: Page,
  worker: TerminalTestWorker,
): Promise<string> {
  const sessionId = await spawnPtyFixtureSession(page, worker);
  await switchToSmokeSession(page, sessionId);
  await waitForPainted(page, sessionId, PTY_FIXTURE_READY);
  return sessionId;
}

/** Browser-local carrier evidence. A direct proof must match the elected route. */
export function readPeerRoute(page: Page, sessionId: string): Promise<PeerRouteReading> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    const snapshot = smokeWindow.__smoke.terminalBrowserSnapshot(id);
    const active = snapshot.route.active;
    const candidate = snapshot.route.candidate;
    const proof = snapshot.replica.last_terminal_proof_generation;
    return {
      activeKind: active?.kind ?? null,
      activeWorkerEpoch: active?.worker_epoch ?? null,
      activePeerId: active?.peer_id ?? null,
      activeProbeAgeMs: active?.probe_age_ms ?? null,
      inputPhase: snapshot.route.input_phase ?? null,
      activeRttMs: active?.rtt_ms ?? null,
      activeBufferedBytes: active?.buffered_bytes ?? null,
      activeCandidateType: active?.candidate_type ?? "none",
      activeWorkerControlRttMs: active?.worker_control_rtt_ms ?? null,
      candidateKind: candidate?.kind ?? null,
      candidateWorkerEpoch: candidate?.worker_epoch ?? null,
      candidatePeerId: candidate?.peer_id ?? null,
      peerPhase: snapshot.route.peer_phase ?? null,
      fallbackReason: snapshot.route.fallback_reason ?? null,
      failureDetail: snapshot.route.failure_detail ?? null,
      proofKind: proof?.transportKind ?? null,
      candidateCandidateType: candidate?.candidate_type ?? "none",
      pendingInputCount: snapshot.route.pending_input_count,
      proofWorkerEpoch: proof?.processEpoch ?? null,
      baselineReady: snapshot.replica.baseline_ready,
      proofSocketId: proof?.socketId ?? null,
      proofSocketGeneration: proof?.socketGeneration ?? null,
      syncReady: snapshot.sync.ready,
    } satisfies PeerRouteReading;
  }, sessionId);
}

/** Waits until an elected direct route has committed a baseline it actually paints. */
export async function waitForDirectRoute(
  page: Page,
  sessionId: string,
  expectedKind: DirectTerminalPeerRouteKind = "webrtc",
  options: { syncMetadata?: boolean } = {},
): Promise<PeerRouteReading> {
  const requiresSyncMetadata = options.syncMetadata ?? true;
  let lastRoute: PeerRouteReading | null = null;
  try {
    await expect.poll(async () => {
      const route = await readPeerRoute(page, sessionId);
      lastRoute = route;
      return route.activeKind === expectedKind
        && route.proofKind === expectedKind
        && route.baselineReady
        && route.activeWorkerEpoch !== null
        && route.proofWorkerEpoch === route.activeWorkerEpoch
        && (expectedKind !== "webrtc" || route.activePeerId !== null)
        && (!requiresSyncMetadata || route.syncReady);
    }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  } catch (error) {
    throw new Error(`direct route unavailable: ${JSON.stringify(lastRoute)}`, { cause: error });
  }
  return readPeerRoute(page, sessionId);
}

/** Waits for the standard Sync fallback to own and paint the current session. */
export async function waitForSyncRoute(page: Page, sessionId: string): Promise<PeerRouteReading> {
  await expect.poll(async () => {
    const route = await readPeerRoute(page, sessionId);
    return route.activeKind === "sync"
      && route.proofKind === "sync"
      && route.baselineReady
      && route.syncReady;
  }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  return readPeerRoute(page, sessionId);
}

/** Waits until route fencing has admitted input on the elected fallback carrier. */
export async function waitForTerminalInputReady(page: Page, sessionId: string): Promise<void> {
  let latest: PeerRouteReading | null = null;
  try {
    await expect.poll(async () => {
      latest = await readPeerRoute(page, sessionId);
      return latest.baselineReady && (
        (latest.activeKind === "sync" && latest.syncReady)
        || (latest.inputPhase === "sending" && latest.pendingInputCount === 0)
      );
    }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  } catch (error) {
    throw new Error(`terminal input fallback unavailable: ${JSON.stringify(latest)}`, { cause: error });
  }
}

/** Waits for a real peer attempt to fail while the existing Sync view remains usable. */
export async function waitForPeerFallback(
  page: Page,
  sessionId: string,
  reason: string,
): Promise<PeerRouteReading> {
  let latest: PeerRouteReading | null = null;
  try {
    await expect.poll(async () => {
      latest = await readPeerRoute(page, sessionId);
      return latest.activeKind === "sync"
        && latest.proofKind === "sync"
        && latest.baselineReady
        && latest.syncReady
        && latest.fallbackReason === reason;
    }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  } catch (error) {
    throw new Error(`peer fallback unavailable: ${JSON.stringify(latest)}`, { cause: error });
  }
  return latest!;
}

/** Captures the exact elected direct identity so a later assertion can reject route churn. */
export function peerRouteIdentity(route: PeerRouteReading): string {
  return [
    route.activeKind,
    route.activeWorkerEpoch,
    route.activePeerId,
    route.proofSocketId,
    route.proofSocketGeneration,
  ].join("/");
}

/** Waits for browser WebRTC stats to report an actual allowed host candidate pair. */
export async function waitForHostPeerTelemetry(
  page: Page,
  sessionId: string,
): Promise<PeerRouteReading> {
  let latest: PeerRouteReading | null = null;
  try {
    await expect.poll(async () => {
      latest = await readPeerRoute(page, sessionId);
      return latest.activeKind === "webrtc"
        && latest.activeCandidateType === "host"
        && latest.activeWorkerControlRttMs !== null
        && latest.activeWorkerControlRttMs >= 0;
    }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  } catch (error) {
    throw new Error(`${String(error)}\nlatest route: ${JSON.stringify(latest)}`);
  }
  return latest!;
}

/** Waits until an authenticated WebRTC connection is staged or has already won election. */
export async function waitForWebRtcCandidate(page: Page, sessionId: string): Promise<PeerRouteReading> {
  let latest: PeerRouteReading | null = null;
  try {
    await expect.poll(async () => {
      latest = await readPeerRoute(page, sessionId);
      return latest.activeKind === "webrtc" || (
        latest.activeKind === "sync"
        && latest.candidateKind === "webrtc"
        && latest.candidateWorkerEpoch !== null
        && latest.candidatePeerId !== null
      );
    }, { timeout: PEER_ROUTE_TIMEOUT_MS, intervals: [...PEER_ROUTE_INTERVALS_MS] }).toBe(true);
  } catch (error) {
    throw new Error(`terminal peer candidate unavailable: ${JSON.stringify(latest)}`, { cause: error });
  }
  return latest!;
}

/** Arms the fixture through the established route, then proves a real trusted key reached its PTY once. */
export async function sendTrustedPeerKey(
  page: Page,
  sessionId: string,
  nonce = crypto.randomUUID(),
): Promise<TrustedPeerKeyResult> {
  const marker = `ACK:${nonce}`;
  const arm = encodePtyFixtureCommand({ op: "ARM_KEY", nonce });
  await page.evaluate(async ({ id, frame }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, frame);
  }, { id: sessionId, frame: arm });
  await waitForPainted(page, sessionId, `ARMED:${nonce}`);
  await page.getByTestId(`terminal-slot-${sessionId}`).click();
  await expect.poll(() => page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.paneFocused(id).focused;
  }, sessionId), { timeout: 10_000, intervals: [50, 100] }).toBe(true);
  const timingId = await page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.beginTerminalTiming("trusted_key", id);
  }, sessionId);
  await page.keyboard.press("x");
  const timing = await page.evaluate(({ id, timingId: idForTiming, expected }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.finishTerminalTiming(idForTiming, id, expected, 30_000);
  }, { id: sessionId, timingId, expected: marker });
  expect(timing.trustedKey).toBe(true);
  await expectMarkersOnce(page, sessionId, [marker]);
  return { nonce, marker, timing };
}

/** Sends a fixture-owned output marker without treating the smoke injection as input proof. */
export async function emitPeerMarker(page: Page, sessionId: string, marker: string): Promise<void> {
  const frame = encodePtyFixtureCommand({ op: "EMIT", text: marker });
  await page.evaluate(async ({ id, command }) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    await smokeWindow.__smoke.input(id, command);
  }, { id: sessionId, command: frame });
  await waitForPainted(page, sessionId, marker);
}

/** Finds the authenticated browser device fingerprint through the coordinator's own UI-state record. */
export async function browserDeviceFingerprint(page: Page, stack: TerminalTestStack): Promise<string> {
  const tabId = await page.evaluate(() => sessionStorage.getItem("roost.tabId"));
  if (!tabId) throw new Error("peer smoke page did not claim a tab identity");
  await expect.poll(async () => {
    const tab = (await stack.client.uiListStates({})).tabs
      .find((candidate) => candidate.tabId === tabId);
    return tab?.fp ?? null;
  }, { timeout: 30_000, intervals: [100, 250, 500] }).toMatch(/^[0-9a-f]{64}$/iu);
  const tab = (await stack.client.uiListStates({})).tabs
    .find((candidate) => candidate.tabId === tabId);
  if (!tab?.fp) throw new Error("peer smoke page lost its coordinator device identity");
  return tab.fp;
}

/** Reads the current active-route snapshot as a strongly named test diagnostic. */
export function browserPeerSnapshot(
  page: Page,
  sessionId: string,
): Promise<TerminalBrowserStreamSnapshot> {
  return page.evaluate((id) => {
    const smokeWindow = window as unknown as { __smoke: RecoverySmokeApi };
    return smokeWindow.__smoke.terminalBrowserSnapshot(id);
  }, sessionId);
}

/** Blocks the coordinator HTTP fallback so a painted history page proves the direct carrier answered. */
export async function blockCoordinatorHistoryFallback(page: Page): Promise<() => Promise<void>> {
  const pattern = "**/roost.v1.CoordinatorService/SessionsGetScrollbackCells";
  await page.route(pattern, (route) => route.abort("failed"));
  return () => page.unroute(pattern);
}
