// Coord-health poller. Polls misc.health every 5 s and writes
// window.__roostCoordHealth, which ConnectionBanner reads every 2 s to decide
// whether to show the "coordinator unreachable" banner (after 3 consecutive
// failures = 15 s grace). Split out of store/sync.ts (400-line cap); fully
// self-contained — coordClient (dynamic import) + window/document only, no
// sync.ts state. Started once by sync.ts bootstrap via _startCoordHealthPoller.

import type { CoordHealthSnapshot } from "../components/ConnectionBanner.tsx";
import { isPageVisible } from "../lib/pageVisible.ts";
import { diag } from "@roost/shared/diag";

const HEALTH_POLL_INTERVAL_MS = 5_000;
const HEALTH_POLL_TIMEOUT_MS = 4_000;
const HEALTH_FAILURE_THRESHOLD = 3;
// Grace window after page load — bootstrap fires 8 concurrent list queries,
// the sync stream connects, and the spawn flow may run; the health poll can
// get starved behind that. Suppress the "unreachable" banner during this
// window so a normal cold-start never shows red. _pollerStartedAt set on boot.
const HEALTH_BANNER_GRACE_MS = 8_000;

let _healthConsecutiveFailures = 0;
let _healthPollInFlight = false;
let _pollerStartedAt = 0;
let _healthPollerStarted = false;

function _writeCoordHealth(snapshot: CoordHealthSnapshot): void {
  (window as Window & { __roostCoordHealth?: CoordHealthSnapshot }).__roostCoordHealth = snapshot;
}

async function _pollCoordHealth(): Promise<void> {
  // Drop overlap: setInterval can fire a new poll before the prior one
  // resolves (slow batch, WS reconnect). Without a guard, _healthConsecutiveFailures
  // increments out of order and the banner flickers.
  if (_healthPollInFlight) return;
  _healthPollInFlight = true;
  try {
    const { coordClient } = await import("../connect.ts");
    await Promise.race([
      coordClient.miscHealth({}),
      new Promise((_, reject) =>
        setTimeout(() => reject(new Error("health poll timeout")), HEALTH_POLL_TIMEOUT_MS),
      ),
    ]);
    _healthConsecutiveFailures = 0;
    _writeCoordHealth({
      lastSuccessMs: performance.now(),
      lastErrorMs: null,
      lastResult: { kind: "ok" },
    });
  } catch (err: unknown) {
    _healthConsecutiveFailures += 1;
    const errMsg = err instanceof Error ? err.message : String(err);
    const isMixedContent =
      errMsg.includes("mixed") || errMsg.includes("blocked") || errMsg.includes("insecure");
    const withinGrace = performance.now() - _pollerStartedAt < HEALTH_BANNER_GRACE_MS;
    // Mark unreachable only once threshold is exceeded AND we're past the
    // cold-start grace window — bootstrap traffic was wrongly painting red.
    if (_healthConsecutiveFailures >= HEALTH_FAILURE_THRESHOLD && !withinGrace) {
      _writeCoordHealth({
        lastSuccessMs:
          (window as Window & { __roostCoordHealth?: CoordHealthSnapshot })
            .__roostCoordHealth?.lastSuccessMs ?? null,
        lastErrorMs: performance.now(),
        lastResult: {
          kind: "unreachable",
          error: isMixedContent ? "mixed-content blocked" : errMsg,
        },
      });
      diag("sync.health_unreachable", { error: isMixedContent ? "mixed-content blocked" : errMsg });
    }
  } finally {
    _healthPollInFlight = false;
  }
}

export function _startCoordHealthPoller(): void {
  if (_healthPollerStarted) return;
  _healthPollerStarted = true;
  _pollerStartedAt = performance.now();
  // Immediate first poll so banner reacts within one tick rather than 5 s.
  void _pollCoordHealth();
  // Skip while tab is hidden — no one's watching the banner; resumes on next tick when visible.
  setInterval(() => { if (isPageVisible()) void _pollCoordHealth(); }, HEALTH_POLL_INTERVAL_MS);
  // Poll the instant the tab is refocused. Without this, lastSuccessMs is as
  // stale as the time the tab spent hidden, so the banner would flash a false
  // "coordinator unreachable" on refocus until the next interval tick.
  document.addEventListener("visibilitychange", () => {
    if (isPageVisible()) { _pollerStartedAt = performance.now(); void _pollCoordHealth(); }
  });
}
