// Smoke and manual-recovery backdoors for the Sync transport, split out of
// store/sync.ts. Each one reaches the transport only through the narrow mutators
// sync.ts exports beside the state they touch, so the redial ladder and the pause
// gate stay single-writer inside _runConnectSync.
//
// The roostSmoke-gated exports additionally require VITE_ROOST_SMOKE=1 at
// BUILD time: production bundles fold them to no-ops, so the localStorage key
// alone cannot steer the transport. forceSyncReconnect is the one export here
// that is NOT gated: it has three live callers in store/sync-bootstrap.ts
// (hydrator retry, coordinator relocation, and a missing terminal snapshot token).

import {
  _armSyncRedialFloor,
  _requestSyncRedial,
  _setSmokeTransportPaused,
  resumeSyncNow,
} from "./sync.ts";

const SMOKE_ARMED = import.meta.env.VITE_ROOST_SMOKE === "1"
  && typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1";


/** Test-only: force-close the live firehose WS so the reconnect loop re-dials. */
export function forceSyncReconnect(): void { _requestSyncRedial(); }

/** Smoke-only: drop the tube exactly as a network failure does, with the failure
 * count pre-armed to the floor production can still reach — saturated capped
 * backoff, plus the sleep while hidden. Recovery uses no backdoor: a visible
 * document must heal on its own capped redial, a hidden one on its next resume. */
export function forceSyncMaxBackoff(): void {
  if (!SMOKE_ARMED) return;
  _armSyncRedialFloor();
}

/** Smoke-only partition gate. Close the current tube and hold re-dial until the
 * paired resume, allowing the real PTY to diverge from the browser consumer. */
export function pauseSyncTransport(): void {
  if (!SMOKE_ARMED) return;
  _setSmokeTransportPaused(true);
  _requestSyncRedial();
}

export function resumeSyncTransport(): void {
  if (!SMOKE_ARMED) return;
  _setSmokeTransportPaused(false);
  resumeSyncNow();
}
