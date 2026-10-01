//! The peer's route READING and the two waits that consume it.
//!
//! Split out of `terminal-peer-helpers.ts`: what a carrier is RIGHT NOW is a
//! different question from how a spec sets one up. The reading is one
//! `page.evaluate` over the browser's own carrier snapshot, and the two waiters
//! are the only things that poll it — so they travel together, and the setup
//! helpers (opening a page, minting a fixture session, sending a trusted key)
//! stay where they were.

import { expect, type Page } from "@playwright/test";

import type { RecoverySmokeApi } from "./terminal-smoke-api.ts";
import type { DirectTerminalPeerRouteKind, PeerRouteReading } from "./terminal-peer-helpers.ts";

export const PEER_ROUTE_TIMEOUT_MS = Number(process.env.ROOST_PEER_ROUTE_TIMEOUT_MS ?? 60_000);
export const PEER_ROUTE_INTERVALS_MS = [100, 250, 500] as const;

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

/**
 * Waits until an elected direct route has committed a baseline it actually paints.
 *
 * `replaced` is for the specs that assert a route CHANGED — a worker restart, a
 * revocation, a failover. Without it the poll succeeds on its first read, which
 * is a perfectly good route and the one the reader already had, so the spec's
 * next assertion compares the old reading with itself and fails on a product
 * that is behaving correctly. Passing the pre-event reading makes the wait mean
 * "and it is not that one", which is what the spec is about; the spec's own
 * `not.toBe` assertions are unchanged and now observe the retirement instead
 * of racing it.
 */
export async function waitForDirectRoute(
  page: Page,
  sessionId: string,
  expectedKind: DirectTerminalPeerRouteKind = "webrtc",
  options: { syncMetadata?: boolean; replaced?: PeerRouteReading } = {},
): Promise<PeerRouteReading> {
  const requiresSyncMetadata = options.syncMetadata ?? true;
  const previous = options.replaced;
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
        && (!requiresSyncMetadata || route.syncReady)
        && (previous === undefined
          || (route.activeWorkerEpoch !== previous.activeWorkerEpoch
            && route.activePeerId !== previous.activePeerId
            && route.proofSocketId !== previous.proofSocketId));
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