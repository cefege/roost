// Bootstrap orders authorization and snapshots behind the subscribed Sync barrier.
// That ordering prevents cold-start events from falling between a list RPC and the socket.
// This module owns retries and worker/identity refresh while sync.ts owns the live tube.
// Snapshot conversion lives in sync-bootstrap-hydration.ts so this lifecycle stays readable.
// Existing callers enter through bootstrapSync and refreshCoordAndWorkers.

import { batch } from "solid-js";
import { reconcile } from "solid-js/store";
import { setRootStore, rootStore } from "./root.ts";
import { signal } from "@roost/shared/diag";
import type { Worker } from "@roost/shared/wire";
import { claimTabIdentity } from "../auth/tab-id.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { _startCoordHealthPoller } from "./sync-health.ts";
import {
  _runConnectSync,
  forceSyncReconnect,
  installSyncLifecycleWake,
  registerSyncAuthRejectionHandler,
  waitForSyncSubscribed,
} from "./sync.ts";
import { createSingleSyncLoopStarter } from "./sync-flow.ts";
import { _dispatchCapturedFragmentCredential } from "./sync-bootstrap.pair.ts";
import { relocateRetiredBrowser } from "../auth/coordinator-relocation.ts";
import { setTerminalBootstrapStage } from "./sync-hydrated.ts";
import { markPhase } from "../lib/diag.ts";
import { _installBootstrapDomainHydrators } from "./sync-bootstrap-hydration.ts";
import {
  bootstrapDashboardAccess,
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
  refreshDashboardAccess,
  suspendDashboardScopedClientState,
} from "./dashboard-selection.ts";


registerSyncAuthRejectionHandler(() => setBrowserUnauthorized(true));

let synced = false;

// Longest bootstrap waits for the v2 subscribed barrier before using one
// protected SessionsList solely to classify first-use auth versus offline.
const SYNC_SUBSCRIBED_WAIT_MS = 3000;

// Route guards consume these leaf signals through the historical sync-bootstrap
// import path; keeping the state in sync-hydrated avoids the firehose cycle.
export {
  sessionsHydrated,
  workersHydrated,
  terminalBootstrapStage,
  type TerminalBootstrapStage,
} from "./sync-hydrated.ts";


export function bootstrapSync(): void {
  if (synced) return;
  synced = true;
  void _bootstrap();
  // Page-lifecycle resume is owned by store/sync.ts: one coalesced wake edge for
  // the one redial loop. Coord can be restarted while the tab sits idle, so the
  // same edge re-fetches coord_identity + workers — without it the SPA carries a
  // stale coord_identity.git_sha and the drift badge silently mis-reports until
  // the next full reload.
  installSyncLifecycleWake(() => { void claimTabIdentity().then(() => refreshCoordAndWorkers()); });
}

/** Re-fetch coord identity + worker list and overwrite the relevant
 *  rootStore slices. Safe to call from focus / post-deploy / after a
 *  reconnect — Promise.allSettled keeps either fetch's failure from
 *  blocking the other. */
export async function refreshCoordAndWorkers(): Promise<void> {
  if (rootStore.coord_identity === null) return;
  const { classifyAuthFailure, coordClient } = await import("../connect.ts");
  const dashboardAccess = await Promise.resolve(refreshDashboardAccess()).then(
    (value) => ({ status: "fulfilled" as const, value }),
    (reason) => ({ status: "rejected" as const, reason }),
  );
  if (dashboardAccess.status === "rejected") {
    setBrowserUnauthorized(
      classifyAuthFailure(
        dashboardAccess.reason,
        "/roost.v1.CoordinatorService/AuthDashboardAccess",
      ) === "device",
    );
    return;
  }
  if (!dashboardAccess.value) return;
  const dashboardToken = captureDashboardResourceToken();
  const identity = await Promise.resolve(coordClient.authCoordIdentity({})).then(
    (value) => ({ status: "fulfilled" as const, value }),
    (reason) => ({ status: "rejected" as const, reason }),
  );
  if (identity.status === "fulfilled" && await relocateRetiredBrowser(identity.value) !== "failed") return;
  const workers = await Promise.resolve(coordClient.workersList({})).then(
    (value) => ({ status: "fulfilled" as const, value }),
    (reason) => ({ status: "rejected" as const, reason }),
  );
  if (!isCurrentDashboardResourceToken(dashboardToken)) return;
  if (identity.status === "fulfilled") {
    setRootStore("coord_identity", {
      git_sha: identity.value.gitSha,
      public_url: identity.value.publicUrl,
      public_listener: identity.value.publicListener,
      saas_mode: identity.value.saasMode,
      relocated_to_url: identity.value.relocatedToUrl,
      handoff_id: identity.value.handoffId,
    });
  }
  // Refresh path mirror of bootstrap's unauth detection: clear or
  // set browser_unauthorized so the sidebar empty-state kind tracks
  // current authorization. Runs on visibility regain + post-deploy + after
  // a pair-approval reload, so the user sees the right empty state.
  setBrowserUnauthorized(
    workers.status === "rejected"
      && classifyAuthFailure(
        workers.reason,
        "/roost.v1.CoordinatorService/WorkersList",
      ) === "device",
  );

  if (workers.status === "fulfilled") {
    setRoutableFps(new Set(workers.value.routableFps));
    const rec: Record<string, Worker> = {};
    for (const w of workers.value.workers) {
      const wire: Worker = {
        fp: w.fp as never,
        label: w.label,
        os: w.os as never,
        git_sha: w.gitSha ?? null,
        host_metrics: w.hostMetrics ? {
          cpu_pct: w.hostMetrics.cpuPct,
          mem_used_bytes: Number(w.hostMetrics.memUsedBytes),
          mem_total_bytes: Number(w.hostMetrics.memTotalBytes),
          disk_used_bytes: Number(w.hostMetrics.diskUsedBytes),
          disk_total_bytes: Number(w.hostMetrics.diskTotalBytes),
          net_rx_bps: Number(w.hostMetrics.netRxBps),
          net_tx_bps: Number(w.hostMetrics.netTxBps),
          sampled_at_ms: Number(w.hostMetrics.sampledAtMs),
        } : null,
        registered_at_ms: Number(w.registeredAtMs),
        last_seen_ms: Number(w.lastSeenMs),
        reachable_addr: w.reachableAddr ?? null,
        keeper_stale: w.keeperStale ?? null,
      };
      rec[wire.fp] = wire;
    }
    // Per-fp reconcile in ONE batch (NOT a whole-record set of all-new
    // objects — that invalidated every workers[fp] subscriber on every tab
    // refocus). No-delete semantics preserved: fps missing from the list
    // are not removed (same as the prior shallow merge).
    batch(() => {
      for (const [fp, w] of Object.entries(rec)) setRootStore("workers", fp, reconcile(w));
    });
  }
}

// Bootstrap retry on TRANSIENT failure (coord briefly unreachable — e.g. the
// SPA reloads during a deploy kickstart before coord is back up). The unary
// list calls then reject with a NETWORK error ("Failed to fetch"), the store
// populates NOTHING, and — unlike the sync stream — the bootstrap never retried,
// leaving the app permanently blank ("can't input") until a manual reload.
// Retry with backoff until coord answers. Unauthenticated is NOT retried (it's
// a real auth state → Onboarding).
let _bootstrapRetries = 0;
let _bootstrapRetryTimer: Timer | null = null;
let _hydratorsInstalled = false;
// `_runConnectSync` owns an infinite reconnect loop. Bootstrap retries share
// this one starter instead of creating competing socket generations.
const _startSyncLoop = createSingleSyncLoopStarter(() => { void _runConnectSync(); });

/** Continue the one bootstrap pipeline immediately after a public password RPC
 * binds this browser key. The single-loop starter still prevents duplicate
 * Sync sockets if a scheduled retry wins the race. */
export function resumeBootstrapAfterDeviceAuthorization(): void {
  setBrowserUnauthorized(false);
  _bootstrapRetries = 0;
  if (_bootstrapRetryTimer) {
    clearTimeout(_bootstrapRetryTimer);
    _bootstrapRetryTimer = null;
  }
  void _bootstrap();
}

function setBrowserUnauthorized(next: boolean): void {
  // Signal only the authorization loss edge; a persistently unknown browser
  // must not emit another relogin event on every visibility refresh.
  if (next && !rootStore.browser_unauthorized) {
    signal("auth.relogin_401", {});
    suspendDashboardScopedClientState();
  }
  setRootStore("browser_unauthorized", next);
}

function _scheduleBootstrapRetry(): void {
  if (_bootstrapRetryTimer) return;
  const delay = Math.min(1000 * 2 ** _bootstrapRetries, 10_000);
  _bootstrapRetries++;
  console.warn("[sync.bootstrap] coord unreachable — retrying in", delay, "ms (attempt", _bootstrapRetries, ")");
  _bootstrapRetryTimer = setTimeout(() => {
    _bootstrapRetryTimer = null;
    if (_hydratorsInstalled) forceSyncReconnect();
    else void _bootstrap();
  }, delay);
}

async function _bootstrap(): Promise<void> {
  await claimTabIdentity();
  setTerminalBootstrapStage("identity");
  try {
    // Fragment credential redemption and identity remain the only serial
    // prerequisites. Every authoritative domain list starts after the v2
    // subscribed control installs this socket's generation tokens.
    // Kept dynamic to preserve the deliberate bootstrap → sync one-way edge:
    // connect.ts reaches the root store and eagerly importing it here creates
    // an initialization cycle during cold module evaluation.
    const {
      classifyAuthFailure,
      coordClient,
      publicCoordClient,
      reconcileCoordinatorOverrideAfterDiscovery,
    } = await import("../connect.ts");
    const initialIdentity = await Promise.resolve(publicCoordClient.authCoordIdentity({})).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason) => ({ status: "rejected" as const, reason }),
    );
    markPhase("identity_complete", { status: initialIdentity.status });
    if (
      initialIdentity.status === "fulfilled"
      && reconcileCoordinatorOverrideAfterDiscovery(initialIdentity.value.saasMode)
    ) {
      location.reload();
      return;
    }
    const selfHosted = initialIdentity.status === "fulfilled"
      && !initialIdentity.value.saasMode;
    if (
      initialIdentity.status === "fulfilled"
      && await relocateRetiredBrowser(initialIdentity.value) !== "failed"
    ) return;
    if (initialIdentity.status === "fulfilled") {
      setRootStore("coord_identity", {
        git_sha: initialIdentity.value.gitSha,
        public_url: initialIdentity.value.publicUrl,
        public_listener: initialIdentity.value.publicListener,
        saas_mode: initialIdentity.value.saasMode,
        relocated_to_url: initialIdentity.value.relocatedToUrl,
        handoff_id: initialIdentity.value.handoffId,
      });
      if (selfHosted) _startCoordHealthPoller();
    }
    if (selfHosted && await _dispatchCapturedFragmentCredential()) return;
    setTerminalBootstrapStage("authorization");
    const dashboardAccess = await Promise.resolve(bootstrapDashboardAccess()).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason) => ({ status: "rejected" as const, reason }),
    );
    if (dashboardAccess.status === "rejected") {
      const deviceRejected = classifyAuthFailure(
        dashboardAccess.reason,
        "/roost.v1.CoordinatorService/AuthDashboardAccess",
      ) === "device";
      if (deviceRejected) {
        setBrowserUnauthorized(true);
        return;
      }
      _scheduleBootstrapRetry();
      return;
    }
    if (!dashboardAccess.value) {
      throw new Error("coordinator returned invalid dashboard access");
    }
    setBrowserUnauthorized(false);
    _startCoordHealthPoller();
    setTerminalBootstrapStage("sync");
    _startSyncLoop();
    const subscribed = await waitForSyncSubscribed(SYNC_SUBSCRIBED_WAIT_MS);
    if (!subscribed) {
      // Never publish this pre-barrier result. It exists only to distinguish a
      // first-use unknown key from an unreachable/old coordinator.
      const probe = await Promise.resolve(coordClient.sessionsList({})).then(
        (value) => ({ status: "fulfilled" as const, value }),
        (reason) => ({ status: "rejected" as const, reason }),
      );
      if (
        probe.status === "rejected"
        && classifyAuthFailure(
          probe.reason,
          "/roost.v1.CoordinatorService/SessionsList",
        ) === "device"
      ) {
        setBrowserUnauthorized(true);
        return;
      }
      if (probe.status === "fulfilled") setBrowserUnauthorized(false);
      _startCoordHealthPoller();
      _scheduleBootstrapRetry();
      return;
    }
    setTerminalBootstrapStage("sessions");
    if (_hydratorsInstalled) return;
    _hydratorsInstalled = true;

    const terminalFailure = async (reason: unknown): Promise<void> => {
      const authFailure = classifyAuthFailure(
        reason,
        "/roost.v1.CoordinatorService/SessionsList",
      );
      if (authFailure === "device") {
        setBrowserUnauthorized(true);
        return;
      }
      _startCoordHealthPoller();
      _scheduleBootstrapRetry();
    };

    // Registration starts every current-generation hydrator synchronously;
    // terminal remains first while all list calls overlap.
    _installBootstrapDomainHydrators({
      coordClient,
      selfHosted,
      onTerminalFailure: terminalFailure,
      requestReconnect: forceSyncReconnect,
      onTerminalSnapshotApplied: (token, sessionCount) => {
        setBrowserUnauthorized(false);
        markPhase("sessions_list_publish", {
          socketGeneration: token.socketGeneration,
          domainGeneration: token.domainGeneration,
          sessions: sessionCount,
        });
        _bootstrapRetries = 0;
        if (_bootstrapRetryTimer) {
          clearTimeout(_bootstrapRetryTimer);
          _bootstrapRetryTimer = null;
        }
        _startCoordHealthPoller();
      },
    });
  } catch (error) {
    console.error("[sync] bootstrap failed", error);
    signal("diag.corruption_signal", {
      kind: "bootstrap_failed",
      msg: String(error),
      cooldownKey: "bootstrap",
    });
    _scheduleBootstrapRetry();
  }
}
