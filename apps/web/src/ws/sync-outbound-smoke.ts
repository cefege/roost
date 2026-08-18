// Smoke backdoors for the terminal outbound lane. Split out of
// ws/sync-outbound.ts so the production input and viewport paths carry only a
// call into this module. Every entry point is gated on the `roostSmoke`
// document flag, so a normal document can neither register an observer nor arm
// a rejection.
//
// `rejectNextViewportClaim` is NOT here: arming it needs the live claim
// watermark from the viewport registry, and this module stays a leaf so the
// registry can clear a session's arms while it evicts. It lives beside the
// dispatch that consumes the arm, in sync-outbound-viewport-dispatch.ts.

import type { SyncV2TerminalState } from "../store/sync.ts";
import type { ViewportDesired, ViewportSession } from "./sync-outbound-viewport-types.ts";

export type SmokeTerminalInputObserver = (sessionId: string, bytes: Uint8Array) => void;

export interface SmokeViewportRejectArm {
  afterSequence: bigint;
  socketId: string | null;
  domainGeneration: bigint | null;
}

let smokeTerminalInputObserver: SmokeTerminalInputObserver | null = null;
const smokeRejectNextViewportClaims = new Map<string, SmokeViewportRejectArm>();
const smokeRejectedViewportClaimCounts = new Map<string, number>();

/** Install the input-admission observer used by the lazy smoke backdoor.
 * Registration is ignored outside a smoke-enabled document, and the observer
 * receives its own byte copy so instrumentation cannot mutate the live batch. */
export function setSmokeTerminalInputObserver(
  observer: SmokeTerminalInputObserver | null,
): void {
  try {
    if (typeof localStorage === "undefined" || localStorage.getItem("roostSmoke") !== "1") return;
  } catch {
    return;
  }
  smokeTerminalInputObserver = observer;
}

/** The installed observer, or null. Read once per admitted input batch. */
export function currentSmokeTerminalInputObserver(): SmokeTerminalInputObserver | null {
  return smokeTerminalInputObserver;
}

export function smokeBackdoorEnabled(): boolean {
  try {
    return typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1";
  } catch {
    return false;
  }
}

/** Arm one rejection for this session. Callers must gate on
 * smokeBackdoorEnabled() first. */
export function armSmokeViewportRejection(sessionId: string, arm: SmokeViewportRejectArm): void {
  smokeRejectNextViewportClaims.set(sessionId, arm);
}

export function rejectedViewportClaimCount(sessionId: string): number {
  return smokeRejectedViewportClaimCounts.get(sessionId) ?? 0;
}

export function consumeSmokeViewportRejection(
  session: ViewportSession,
  desired: ViewportDesired,
  sync: SyncV2TerminalState,
): boolean {
  if (desired.cols <= 0 || desired.rows <= 0 || smokeRejectNextViewportClaims.size === 0) return false;
  const armed = smokeRejectNextViewportClaims.get(session.sessionId);
  if (!armed
    || desired.sequence <= armed.afterSequence
    || (armed.socketId !== null && armed.socketId !== sync.socketId)
    || (armed.domainGeneration !== null && armed.domainGeneration !== sync.domainGeneration)) return false;
  smokeRejectNextViewportClaims.delete(session.sessionId);
  smokeRejectedViewportClaimCounts.set(
    session.sessionId,
    (smokeRejectedViewportClaimCounts.get(session.sessionId) ?? 0) + 1,
  );
  return true;
}

/** Drop every smoke record for a session that is being evicted or closed. */
export function forgetSmokeViewportSession(sessionId: string): void {
  smokeRejectNextViewportClaims.delete(sessionId);
  smokeRejectedViewportClaimCounts.delete(sessionId);
}

export function _resetSmokeOutboundForTest(): void {
  smokeRejectNextViewportClaims.clear();
  smokeRejectedViewportClaimCounts.clear();
  smokeTerminalInputObserver = null;
}
