// A visible browser must always have one live Sync dial or one scheduled replacement.
// This module owns capped backoff, hidden-page parking, and coalesced lifecycle wakes.
// The websocket loop asks it when to dial while link state supplies the current socket.
// Keeping every wake flag here prevents reconnect paths from racing socket generations.

import { signal } from "@roost/shared/diag";
import { isPageVisible } from "../lib/pageVisible.ts";
import {
  nextRedialDelayMs,
  shouldCloseStaleLinkOnResume,
  shouldParkRedial,
  SYNC_HIDDEN_PARK_FAILURES,
  SYNC_REDIAL_BASE_MS,
  type SyncLinkLiveness,
} from "./sync-watchdog.ts";
import { isImmediateSyncRedial } from "./sync-flow.ts";
import {
  _closeFailedSyncLink,
  _currentLiveSyncLink,
  _initiateSyncClose,
  _syncLinkIdleMs,
  _syncLinkLiveness,
  type SyncAbortReason,
} from "./sync-link-state.ts";

export interface SyncRedialStatus {
  /** Consecutive failed dials since this tab last received a Sync frame. */
  readonly failures: number;
  /** Delay the pending redial waits — capped, never unbounded. */
  readonly nextDelayMs: number;
  /** True only while a hidden document sleeps instead of redialing. */
  readonly hiddenParked: boolean;
  /** Whether this tab currently has an open socket, a dial in flight, or none. */
  readonly liveness: SyncLinkLiveness;
}

let syncFailures = 0;
let redialDelayMs = SYNC_REDIAL_BASE_MS;
let hiddenParked = false;
let wakeHiddenPark: (() => void) | null = null;
let resumeRequested = false;
let wakeBackoff: (() => void) | null = null;
let smokeTransportPaused = false;
let resumeSmokeTransport: (() => void) | null = null;
const RESUME_COALESCE_MS = 500;
let lastResumeAt = Number.NEGATIVE_INFINITY;

/** Smoke-only transport gate driven from sync-smoke.ts. */
export function _setSmokeTransportPaused(paused: boolean): void {
  smokeTransportPaused = paused;
  if (paused) return;
  resumeSmokeTransport?.();
  resumeSmokeTransport = null;
}

/** Smoke-only: pre-arm the highest redial floor production can reach. */
export function _armSyncRedialFloor(): void {
  syncFailures = SYNC_HIDDEN_PARK_FAILURES - 1;
  redialDelayMs = nextRedialDelayMs(SYNC_HIDDEN_PARK_FAILURES);
  const link = _currentLiveSyncLink();
  if (link) _closeFailedSyncLink(link);
}

/** Redial status for diagnostics and the smoke seam. */
export function syncRedialStatus(): SyncRedialStatus {
  return {
    failures: syncFailures,
    nextDelayMs: redialDelayMs,
    hiddenParked,
    liveness: _syncLinkLiveness(),
  };
}

export function _syncDialAttempt(): number {
  return syncFailures + 1;
}

/** Manually replace a live stale socket without remounting the SPA. */
export function reconnectNow(): void {
  console.info("[sync.connect] manual reconnect requested");
  resumeSyncNow();
  _initiateSyncClose("manual");
}

/** Wake both a hidden park and an ordinary backoff for an immediate dial. */
export function resumeSyncNow(): void {
  hiddenParked = false;
  syncFailures = 0;
  redialDelayMs = SYNC_REDIAL_BASE_MS;
  resumeRequested = true;
  const wakePark = wakeHiddenPark;
  wakeHiddenPark = null;
  wakePark?.();
  const pendingBackoff = wakeBackoff;
  wakeBackoff = null;
  pendingBackoff?.();
}

export function installSyncLifecycleWake(onResume: () => void): () => void {
  if (typeof document === "undefined" || typeof window === "undefined") {
    return () => { /* non-DOM host: nothing to resume */ };
  }
  const wake = (allowHidden: boolean): void => {
    // A hidden pageshow/focus must not consume the following visibility wake.
    // Network restoration may release a hidden park before the page is revealed.
    if (!allowHidden && !isPageVisible()) return;
    const now = performance.now();
    if (now - lastResumeAt < RESUME_COALESCE_MS) return;
    lastResumeAt = now;
    onResume();
    resumeSyncNow();
    // A connecting socket is already the redial. Replace only an open socket
    // that has actually gone silent past the foreground liveness budget.
    if (
      isPageVisible()
      && shouldCloseStaleLinkOnResume(_syncLinkLiveness(), _syncLinkIdleMs())
    ) {
      _initiateSyncClose("visibility");
    }
  };
  const visibleWake = (): void => wake(false);
  const onlineWake = (): void => wake(true);
  document.addEventListener("visibilitychange", visibleWake);
  document.addEventListener("resume", visibleWake);
  window.addEventListener("pageshow", visibleWake);
  window.addEventListener("focus", visibleWake);
  window.addEventListener("online", onlineWake);
  return () => {
    document.removeEventListener("visibilitychange", visibleWake);
    document.removeEventListener("resume", visibleWake);
    window.removeEventListener("pageshow", visibleWake);
    window.removeEventListener("focus", visibleWake);
    window.removeEventListener("online", onlineWake);
  };
}

export function _waitForSyncDialPermission(): Promise<void> | null {
  if (!hiddenParked && !smokeTransportPaused) {
    if (resumeRequested) {
      resumeRequested = false;
      redialDelayMs = SYNC_REDIAL_BASE_MS;
    }
    return null;
  }
  return waitForSyncDialPermission();
}

export function _noteSyncV2FrameReceived(): void {
  syncFailures = 0;
  redialDelayMs = SYNC_REDIAL_BASE_MS;
}

export function _waitForNextSyncDial(
  abortReason: SyncAbortReason,
): Promise<void> | null {
  if (isImmediateSyncRedial(abortReason)) {
    redialDelayMs = SYNC_REDIAL_BASE_MS;
    return null;
  }
  syncFailures += 1;
  redialDelayMs = nextRedialDelayMs(syncFailures);
  const parking = shouldParkRedial(syncFailures);
  if (syncFailures === SYNC_HIDDEN_PARK_FAILURES) {
    console.warn(`[sync.connect] ${syncFailures} consecutive failures`, { parking });
    signal("reconnect.give_up", {
      failures: syncFailures,
      action: parking ? "hidden_park" : "keep_retrying",
      cooldownKey: "sync",
    });
  }
  if (parking) {
    hiddenParked = true;
    return null;
  }
  // A resume may land before this sleep starts or while it is parked. Checking
  // on both sides preserves the wake regardless of that ordering.
  if (resumeRequested) {
    resumeRequested = false;
    redialDelayMs = SYNC_REDIAL_BASE_MS;
    return null;
  }
  return waitForSyncBackoff();
}

async function waitForSyncDialPermission(): Promise<void> {
  if (hiddenParked) {
    // The flag is consumed here so a resume between the failure and this park
    // cannot be lost; resumeSyncNow clears it and resolves this exact waiter.
    console.warn("[sync.connect] hidden and unreachable — sleeping until resume");
    const { promise: resumed, resolve } = Promise.withResolvers<void>();
    wakeHiddenPark = resolve;
    await resumed;
    if (wakeHiddenPark === resolve) wakeHiddenPark = null;
  }
  if (smokeTransportPaused) {
    const { promise: resumed, resolve } = Promise.withResolvers<void>();
    resumeSmokeTransport = resolve;
    await resumed;
    if (resumeSmokeTransport === resolve) resumeSmokeTransport = null;
  }
  if (resumeRequested) {
    resumeRequested = false;
    redialDelayMs = SYNC_REDIAL_BASE_MS;
  }
}

async function waitForSyncBackoff(): Promise<void> {
  const { promise: slept, resolve: wake } = Promise.withResolvers<void>();
  wakeBackoff = wake;
  const sleepTimer = setTimeout(wake, redialDelayMs);
  await slept;
  clearTimeout(sleepTimer);
  wakeBackoff = null;
  if (resumeRequested) {
    resumeRequested = false;
    redialDelayMs = SYNC_REDIAL_BASE_MS;
  }
}
