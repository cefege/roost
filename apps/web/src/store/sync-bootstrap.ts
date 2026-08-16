// Bootstrap + auth/pair + worker refresh — the "before the live stream opens"
// half of sync. Split out of store/sync.ts (400-line cap). One-directional:
// imports the firehose entry points (_runConnectSync, _abortSyncForVisibility)
// from sync.ts; sync.ts imports nothing back. Consumers (App.tsx,
// DeployConsoleModal) import bootstrapSync / refreshCoordAndWorkers from here.

import { batch, createSignal } from "solid-js";
import { reconcile } from "solid-js/store";
import { setRootStore, rootStore } from "./root.ts";
import type { PairRequest } from "./root.ts";
import { signal, diag } from "@roost/shared/diag";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import type { SessionsListResponse } from "@roost/shared/proto/coordinator_pb";
import { sessionFromProto } from "@roost/shared/wire/session-proto";
import type { Worker, Session, Workspace, Task, PermissionRule, McpRelay } from "@roost/shared/wire";
import { getPublicKeyB64, persistedWebKeyAtStartup } from "../auth/web-key.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { _startCoordHealthPoller } from "./sync-health.ts";
import {
  _runConnectSync,
  _abortSyncForVisibility,
  forceSyncReconnect,
  isSyncPaused,
  registerSyncDomainHydrator,
  resumeSyncNow,
  syncLinkIdleMs,
  waitForSyncSubscribed,
} from "./sync.ts";
import { shouldRedialOnRefocus } from "./sync-watchdog.ts";
import { createSingleSyncLoopStarter } from "./sync-flow.ts";
import { _dispatchFragmentCredential } from "./sync-bootstrap.pair.ts";
import { relocateRetiredBrowser } from "../auth/coordinator-relocation.ts";
import { isPageVisible } from "../lib/pageVisible.ts";
import { setSessionsHydrated } from "./sync-hydrated.ts";
import { markPhase } from "../lib/diag.ts";

// Set browser_unauthorized; emit the auth.relogin_401 signal ONLY on the
// rising edge (authed → unauth) so a persistently-unpaired browser doesn't
// re-signal on every visibility-regain poll. The daily digest then shows
// how often a device drops to unpaired (the iOS key-eviction churn).
function setBrowserUnauthorized(next: boolean): void {
  if (next && !rootStore.browser_unauthorized) signal("auth.relogin_401", {});
  setRootStore("browser_unauthorized", next);
}

let synced = false;

// Longest bootstrap waits for the v2 subscribed barrier before using one
// protected SessionsList solely to classify first-use auth versus offline.
const SYNC_SUBSCRIBED_WAIT_MS = 3000;

// True once the Phase-1 `sessionsList` has populated the store at least once.
// MainPane's dead-URL safety net reads this to tell "still bootstrapping"
// (don't bounce a deep link yet) from "genuinely dead" (bounce home). The
// signal itself lives in the leaf sync-hydrated.ts so the firehose can read it
// without an import cycle; re-exported here because that is where consumers
// have always imported it from.
export { sessionsHydrated } from "./sync-hydrated.ts";


// ─── self-register ────────────────────────────────────────────────────────────

async function _attemptSelfRegister(): Promise<void> {
  markPhase("self_register_start");
  try {
    const ssh_pubkey_b64 = await getPublicKeyB64();
    const { coordClient } = await import("../connect.ts");
    await coordClient.authAuthorizeBrowser({ sshPubkeyB64: ssh_pubkey_b64, label: "web-browser" });
    markPhase("self_register_complete", { status: "fulfilled" });
  } catch (err) {
    // PermissionDenied = non-loopback caller → expected; Onboarding
    // handles the explicit-token redeem flow. Any other code (coord
    // down, IDB failed, WebCrypto unavailable, malformed response) is
    // surprising and should surface so it's diagnosable from console.
    // Connect-ES throws ConnectError with `.code`, not the tRPC-era
    // `err.data.httpStatus` — checking the latter never matched and
    // every loopback-deny showed up as a noisy warn in console.
    const { ConnectError, Code } = await import("@connectrpc/connect");
    const isExpected = err instanceof ConnectError && err.code === Code.PermissionDenied;
    markPhase("self_register_complete", {
      status: isExpected ? "denied" : "rejected",
    });
    if (!isExpected) {
      console.warn("[sync] self-register failed (unexpected)", err);
    }
  }
}

export function bootstrapSync(): void {
  if (synced) return;
  synced = true;
  _startCoordHealthPoller();
  void _bootstrap();
  // Re-fetch coord_identity + workers on tab focus. Coord can be
  // restarted while the tab sits idle; without this, the SPA carries
  // a stale coord_identity.git_sha and the drift badge silently
  // mis-reports until the next full reload.
  if (typeof document !== "undefined") {
    document.addEventListener("visibilitychange", () => {
      if (!isPageVisible()) return;
      void refreshCoordAndWorkers();
      // A bounded-retry exhaustion parks the one owning Sync loop. Wake it in
      // place so the persistent terminal deck and its painted grid survive.
      if (isSyncPaused()) { resumeSyncNow(); return; }
      // Keep a live socket across a tab switch: re-dialing costs a JWT sign, a
      // TLS handshake and the since= event backfill, all ahead of the
      // terminal's reveal snapshot. Re-dial only when the link has actually
      // gone silent (a suspended or half-open socket).
      if (shouldRedialOnRefocus(syncLinkIdleMs())) _abortSyncForVisibility();
    });
  }
}

/** Re-fetch coord identity + worker list and overwrite the relevant
 *  rootStore slices. Safe to call from focus / post-deploy / after a
 *  reconnect — Promise.allSettled keeps either fetch's failure from
 *  blocking the other. */
export async function refreshCoordAndWorkers(): Promise<void> {
  const { classifyAuthFailure, coordClient } = await import("../connect.ts");
  const identity = await Promise.resolve(coordClient.authCoordIdentity({})).then(
    (value) => ({ status: "fulfilled" as const, value }),
    (reason) => ({ status: "rejected" as const, reason }),
  );
  if (identity.status === "fulfilled" && await relocateRetiredBrowser(identity.value) !== "failed") return;
  const workers = await Promise.resolve(coordClient.workersList({})).then(
    (value) => ({ status: "fulfilled" as const, value }),
    (reason) => ({ status: "rejected" as const, reason }),
  );
  if (identity.status === "fulfilled") {
    setRootStore("coord_identity", {
      fingerprint_hex: identity.value.fingerprintHex,
      git_sha: identity.value.gitSha,
      public_url: identity.value.publicUrl,
      public_listener: identity.value.publicListener,
      relocated_to_url: identity.value.relocatedToUrl,
      handoff_id: identity.value.handoffId,
    });
  }
  // Refresh path mirror of bootstrap's unauth detection: clear or
  // set browser_unauthorized so the sidebar empty-state kind tracks
  // current trust. Runs on visibility regain + post-deploy + after
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
let _bootstrapRetryTimer: ReturnType<typeof setTimeout> | null = null;
let _hydratorsInstalled = false;
// `_runConnectSync` owns an infinite reconnect loop. Bootstrap retries share
// this one starter instead of creating competing socket generations.
const _startSyncLoop = createSingleSyncLoopStarter(() => { void _runConnectSync(); });
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
  const firstUseWebKey = !persistedWebKeyAtStartup;
  try {
    // Fragment credential redemption and identity remain the only serial
    // prerequisites. Every authoritative domain list starts after the v2
    // subscribed control installs this socket's generation tokens.
    if (await _dispatchFragmentCredential()) return;
    // Kept dynamic to preserve the deliberate bootstrap → sync one-way edge:
    // connect.ts reaches the root store and eagerly importing it here creates
    // an initialization cycle during cold module evaluation.
    const { classifyAuthFailure, coordClient } = await import("../connect.ts");
    const initialIdentity = await Promise.resolve(coordClient.authCoordIdentity({})).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason) => ({ status: "rejected" as const, reason }),
    );
    markPhase("identity_complete", { status: initialIdentity.status });
    if (
      initialIdentity.status === "fulfilled"
      && await relocateRetiredBrowser(initialIdentity.value) !== "failed"
    ) return;
    if (initialIdentity.status === "fulfilled") {
      setRootStore("coord_identity", {
        fingerprint_hex: initialIdentity.value.fingerprintHex,
        git_sha: initialIdentity.value.gitSha,
        public_url: initialIdentity.value.publicUrl,
        public_listener: initialIdentity.value.publicListener,
        relocated_to_url: initialIdentity.value.relocatedToUrl,
        handoff_id: initialIdentity.value.handoffId,
      });
    }
    // A loopback coordinator can authorize a first-use browser immediately.
    // Do it before opening Sync: otherwise the guaranteed 401 consumes the
    // three-second subscribed timeout before bootstrap performs this same RPC.
    markPhase("self_register_gate", {
      firstUseWebKey,
      identityStatus: initialIdentity.status,
    });
    if (firstUseWebKey && initialIdentity.status === "fulfilled") {
      await _attemptSelfRegister();
    }


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
        await _attemptSelfRegister();
        resumeSyncNow();
      } else if (probe.status === "fulfilled") {
        setBrowserUnauthorized(false);
      }
      _startCoordHealthPoller();
      _scheduleBootstrapRetry();
      return;
    }
    if (_hydratorsInstalled) return;
    _hydratorsInstalled = true;

    const terminalFailure = async (reason: unknown): Promise<void> => {
      const authFailure = classifyAuthFailure(
        reason,
        "/roost.v1.CoordinatorService/SessionsList",
      );
      if (authFailure === "device") {
        setBrowserUnauthorized(true);
        await _attemptSelfRegister();
        resumeSyncNow();
        forceSyncReconnect();
        return;
      }
      _startCoordHealthPoller();
      _scheduleBootstrapRetry();
    };

    // Registering a hydrator synchronously starts it for the current token.
    // Terminal goes first, but all calls are in flight before any can settle.
    registerSyncDomainHydrator(SyncDomain.TERMINAL, async (token) => {
      let response: SessionsListResponse;
      try {
        response = await coordClient.sessionsList({ syncSocketId: token.socketId });
      } catch (reason) {
        await terminalFailure(reason);
        return null;
      }
      if (!response.syncSnapshotToken) {
        diag("sync.snapshot_token_missing", { domain: "terminal" });
        forceSyncReconnect();
        return null;
      }
      const sessions: Record<string, Session> = {};
      for (const session of response.sessions) {
        try {
          sessions[session.id] = sessionFromProto(session);
        } catch (error) {
          console.warn("[sync.bootstrap] session_from_proto_failed", session.id, error);
          diag("sync.session_from_proto_failed", {
            error: String(error),
            sid: session.id,
          });
        }
      }
      return {
        snapshotToken: response.syncSnapshotToken,
        apply: () => {
          setRootStore("sessions", sessions);
          setSessionsHydrated(true);
          setBrowserUnauthorized(false);
          markPhase("sessions_list_publish", {
            socketGeneration: token.socketGeneration,
            domainGeneration: token.domainGeneration,
            sessions: Object.keys(sessions).length,
          });
          _bootstrapRetries = 0;
          if (_bootstrapRetryTimer) {
            clearTimeout(_bootstrapRetryTimer);
            _bootstrapRetryTimer = null;
          }
          _startCoordHealthPoller();
        },
      };
    });

    registerSyncDomainHydrator(SyncDomain.WORKERS, async () => {
      const response = await coordClient.workersList({});
      const workers: Record<string, Worker> = {};
      for (const worker of response.workers) {
        workers[worker.fp] = {
          fp: worker.fp as never,
          label: worker.label,
          os: worker.os as never,
          git_sha: worker.gitSha ?? null,
          host_metrics: worker.hostMetrics ? {
            cpu_pct: worker.hostMetrics.cpuPct,
            mem_used_bytes: Number(worker.hostMetrics.memUsedBytes),
            mem_total_bytes: Number(worker.hostMetrics.memTotalBytes),
            disk_used_bytes: Number(worker.hostMetrics.diskUsedBytes),
            disk_total_bytes: Number(worker.hostMetrics.diskTotalBytes),
            net_rx_bps: Number(worker.hostMetrics.netRxBps),
            net_tx_bps: Number(worker.hostMetrics.netTxBps),
            sampled_at_ms: Number(worker.hostMetrics.sampledAtMs),
          } : null,
          registered_at_ms: Number(worker.registeredAtMs),
          last_seen_ms: Number(worker.lastSeenMs),
          reachable_addr: worker.reachableAddr ?? null,
          keeper_stale: worker.keeperStale ?? null,
        };
      }
      return {
        apply: () => {
          setRoutableFps(new Set(response.routableFps));
          setRootStore("workers", workers);
        },
      };
    });

    registerSyncDomainHydrator(SyncDomain.WORKSPACES, async () => {
      const response = await coordClient.workspacesList({});
      const workspaces: Record<string, Workspace> = {};
      for (const workspace of response.workspaces) {
        workspaces[workspace.id] = {
          id: workspace.id as never,
          worker_fp: workspace.workerFp as never,
          name: workspace.name,
          folder_path: workspace.folderPath,
          color: workspace.color ?? null,
          position: workspace.position,
          version: Number(workspace.version),
          created_at_ms: Number(workspace.createdAtMs),
          updated_at_ms: Number(workspace.updatedAtMs),
          session_ids: workspace.sessionIds as never,
        };
      }
      return { apply: () => setRootStore("workspaces", workspaces) };
    });

    registerSyncDomainHydrator(SyncDomain.TASKS, async () => {
      const response = await coordClient.tasksList({});
      const tasks: Record<string, Task> = {};
      for (const task of response.tasks) {
        tasks[task.id] = {
          id: task.id as never,
          state: task.state as never,
          payload: JSON.parse(task.payloadJson),
          enqueued_at_ms: Number(task.enqueuedAtMs),
          claimed_at_ms: task.claimedAtMs !== undefined ? Number(task.claimedAtMs) : null,
          claimed_by: (task.claimedBy ?? null) as never,
          finished_at_ms: task.finishedAtMs !== undefined ? Number(task.finishedAtMs) : null,
          result: task.resultJson ? JSON.parse(task.resultJson) : null,
          completion_check: task.completionCheck ?? null,
          completion_check_last_attempt_ms: task.completionCheckLastAttemptMs !== undefined
            ? Number(task.completionCheckLastAttemptMs)
            : null,
          claim_ttl_ms: Number(task.claimTtlMs),
        };
      }
      return { apply: () => setRootStore("tasks", tasks) };
    });

    registerSyncDomainHydrator(SyncDomain.PERMISSIONS, async () => {
      const response = await coordClient.permissionsList({});
      const rules: Record<string, PermissionRule> = {};
      for (const rule of response.rules) {
        rules[rule.id] = {
          id: rule.id as never,
          tool_pattern: rule.toolPattern,
          folder_glob: rule.folderGlob,
          decision: rule.decision as never,
          enabled: rule.enabled,
          created_at_ms: Number(rule.createdAtMs),
        };
      }
      return { apply: () => setRootStore("permission_rules", rules) };
    });

    registerSyncDomainHydrator(SyncDomain.MCP, async () => {
      const response = await coordClient.mcpList({});
      const relays: Record<string, McpRelay> = {};
      for (const relay of response.relays) {
        relays[relay.id] = {
          id: relay.id as never,
          label: relay.label,
          kind: relay.kind as never,
          config: JSON.parse(relay.configJson),
          created_at_ms: Number(relay.createdAtMs),
        };
      }
      return { apply: () => setRootStore("mcp_relays", relays) };
    });

    registerSyncDomainHydrator(SyncDomain.PAIR, async () => {
      const response = await coordClient.pairList({});
      const requests: Record<string, PairRequest> = {};
      for (const request of response.requests) {
        requests[request.ephemeralId] = {
          ephemeral_id: request.ephemeralId,
          label: request.label,
          created_at_ms: Number(request.createdAtMs),
        };
      }
      return { apply: () => setRootStore("pair_requests", requests) };
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
