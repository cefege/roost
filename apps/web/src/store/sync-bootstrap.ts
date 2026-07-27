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
import { sessionFromProto } from "@roost/shared/wire/agent-proto";
import type { Worker, Session, Workspace, Task, PermissionRule, McpRelay } from "@roost/shared/wire";
import { getPublicKeyB64 } from "../auth/web-key.ts";
import { setRoutableFps } from "./sync-routable.ts";
import { _startCoordHealthPoller } from "./sync-health.ts";
import { _runConnectSync, _abortSyncForVisibility, isSyncPaused } from "./sync.ts";
import { _attemptPairRedeem } from "./sync-bootstrap.pair.ts";
import { relocateRetiredBrowser } from "../auth/coordinator-relocation.ts";
import { isPageVisible } from "../lib/pageVisible.ts";

// Set browser_unauthorized; emit the auth.relogin_401 signal ONLY on the
// rising edge (authed → unauth) so a persistently-unpaired browser doesn't
// re-signal on every visibility-regain poll. The daily digest then shows
// how often a device drops to unpaired (the iOS key-eviction churn).
function setBrowserUnauthorized(next: boolean): void {
  if (next && !rootStore.browser_unauthorized) signal("auth.relogin_401", {});
  setRootStore("browser_unauthorized", next);
}

let synced = false;

// True once the Phase-1 `sessionsList` has populated the store at least once.
// MainPane's dead-URL safety net reads this to tell "still bootstrapping"
// (don't bounce a deep link yet) from "genuinely dead" (bounce home). Plain
// module-level signal — reactive, no owner needed.
const [sessionsHydrated, setSessionsHydrated] = createSignal(false);
export { sessionsHydrated };


// ─── self-register ────────────────────────────────────────────────────────────

async function _attemptSelfRegister(): Promise<void> {
  try {
    const ssh_pubkey_b64 = await getPublicKeyB64();
    const { coordClient } = await import("../connect.ts");
    await coordClient.authAuthorizeBrowser({ sshPubkeyB64: ssh_pubkey_b64, label: "web-browser" });
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
      // A4: if the bounded-retry loop already gave up (SYNC_MAX_FAILURES →
      // _syncPaused, no live stream), refocus MUST re-arm it. _abortSync-
      // ForVisibility is a no-op when paused (nothing to abort), so without
      // this the tab stays stale until a manual Reconnect tap — exactly the
      // iOS-suspend / partition wedge. On refocus, a page reload re-arms
      // the sync loop.
      // Background tabs that hit the retry cap are paused; on refocus,
      // reload to get a clean TLS + HTTP/2 session with the current coord.
      if (isSyncPaused()) { location.reload(); return; }
      // Otherwise: Chrome stalls long-lived HTTP/2 server-streams when the
      // tab sits in the background — silent: no FIN, no error to await.
      // Force a reconnect so any queued events (PTY bytes especially)
      // backfill via sinceEventId.
      _abortSyncForVisibility();
    });
  }
}

/** Re-fetch coord identity + worker list and overwrite the relevant
 *  rootStore slices. Safe to call from focus / post-deploy / after a
 *  reconnect — Promise.allSettled keeps either fetch's failure from
 *  blocking the other. */
export async function refreshCoordAndWorkers(): Promise<void> {
  const { coordClient } = await import("../connect.ts");
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
      relocated_to_url: identity.value.relocatedToUrl,
      handoff_id: identity.value.handoffId,
    });
  }
  // Refresh path mirror of bootstrap's unauth detection: clear or
  // set browser_unauthorized so the sidebar empty-state kind tracks
  // current trust. Runs on visibility regain + post-deploy + after
  // a pair-approval reload, so the user sees the right empty state.
  {
    const { ConnectError, Code } = await import("@connectrpc/connect");
    const isUnauth = workers.status === "rejected" &&
      workers.reason instanceof ConnectError &&
      workers.reason.code === Code.Unauthenticated;
    setBrowserUnauthorized(isUnauth);
  }

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
function _scheduleBootstrapRetry(): void {
  if (_bootstrapRetryTimer) return;
  const delay = Math.min(1000 * 2 ** _bootstrapRetries, 10_000);
  _bootstrapRetries++;
  console.warn("[sync.bootstrap] coord unreachable — retrying in", delay, "ms (attempt", _bootstrapRetries, ")");
  _bootstrapRetryTimer = setTimeout(() => { _bootstrapRetryTimer = null; void _bootstrap(); }, delay);
}

async function _bootstrap(): Promise<void> {
  try {
    // Phase -1: redeem a ?pair token if present (FQDN auth path for the
    // installer-opened host browser + QR-scanning phone). Halts on success
    // (reloads authed).
    if (await _attemptPairRedeem()) return;

    // Phase 0: discover a retired source before any mutation or bulk list.
    // Its mint endpoint can carry this browser to the committed coordinator.
    const { coordClient } = await import("../connect.ts");
    const initialIdentity = await Promise.resolve(coordClient.authCoordIdentity({})).then(
      (value) => ({ status: "fulfilled" as const, value }),
      (reason) => ({ status: "rejected" as const, reason }),
    );
    if (initialIdentity.status === "fulfilled" && await relocateRetiredBrowser(initialIdentity.value) !== "failed") return;

    // Phase 1: attempt loopback browser self-register. If coord recognizes
    // our kid already (cached from a previous boot), this is a no-op upsert.
    // If we're behind a non-loopback proxy, the mutation 403s and we fall
    // through to Onboarding — render path picks up zero-workers + 401 list.
    await _attemptSelfRegister();

    // Phase 2: bulk lists via Connect.
    const [workers, sessions, workspaces, tasks, permRules, mcpRelays, identity, pairRequests] =
      await Promise.allSettled([
        coordClient.workersList({}),
        coordClient.sessionsList({}),
        coordClient.workspacesList({}),
        coordClient.tasksList({}),
        coordClient.permissionsList({}),
        coordClient.mcpList({}),
        coordClient.authCoordIdentity({}),
        coordClient.pairList({}),
      ]);
    // BEFORE the retry gate below: on a retired source every authed list
    // rejects with a non-Unauthenticated error, so `wFail && sFail` returns
    // early — and relocated_to_url would never be stored, leaving
    // ConnectionBanner's "Open new coordinator" button permanently unrendered.
    if (identity.status === "fulfilled") {
      setRootStore("coord_identity", {
        fingerprint_hex: identity.value.fingerprintHex,
        git_sha: identity.value.gitSha,
        public_url: identity.value.publicUrl,
        relocated_to_url: identity.value.relocatedToUrl,
        handoff_id: identity.value.handoffId,
      });
    }

    // Detect "this browser isn't trusted by the coord" — Connect-ES
     // throws ConnectError with `code: "unauthenticated"` on a missing /
     // invalid / unknown-kid JWT. If ANY of the authed list calls came
     // back unauthenticated, surface it as `browser_unauthorized` so
     // AllView renders the `browser-unpaired` empty state with a CTA
     // to /pair (Onboarding) instead of the misleading `no-machines`
     // empty state. Cross-Mac access path. coord_identity + pairList
     // are public — don't count them.
     {
       const { ConnectError, Code } = await import("@connectrpc/connect");
       const authedSettleds = [workers, sessions, workspaces, tasks, permRules, mcpRelays];
       const sawUnauth = authedSettleds.some(
         (r) => r.status === "rejected" &&
           r.reason instanceof ConnectError &&
           r.reason.code === Code.Unauthenticated,
       );
       setBrowserUnauthorized(sawUnauth);

       // Transient coord-unreachable (network reject, NOT auth) → retry the
       // whole bootstrap with backoff. Both critical lists rejected for a
       // non-auth reason = coord down → the store is empty → app blank. This
       // is the "blank/can't input after a deploy" bug. Auth-rejection is a
       // real state (Onboarding), so don't retry it.
       const wFail = workers.status === "rejected";
       const sFail = sessions.status === "rejected";
       if (!sawUnauth && (wFail || sFail)) {
         _scheduleBootstrapRetry();
         if (wFail && sFail) return; // nothing to populate this round
         // partial failure: fall through to populate whatever succeeded
       } else {
         _bootstrapRetries = 0;
         if (_bootstrapRetryTimer) { clearTimeout(_bootstrapRetryTimer); _bootstrapRetryTimer = null; }
       }
     }

     if (workers.status === "fulfilled") {
      setRoutableFps(new Set(workers.value.routableFps));
      const rec: Record<string, Worker> = {};
      for (const w of workers.value.workers) {
        rec[w.fp] = {
          fp: w.fp as never, label: w.label, os: w.os as never,
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
      }
      // Per-fp reconcile in ONE batch — same rationale as
      // refreshCoordAndWorkers above; keeps no-delete semantics.
      batch(() => {
        for (const [fp, w] of Object.entries(rec)) setRootStore("workers", fp, reconcile(w));
      });
    }
    if (sessions.status === "fulfilled") {
      const rec: Record<string, Session> = {};
      for (const s of sessions.value.sessions) {
        // sessionFromProto validates each complete session row. A bad row must
        // not abort bootstrap for workers, workspaces, tasks, or permissions;
        // skip it and surface the same validation failure from live projection.
        try {
          rec[s.id] = sessionFromProto(s);
        } catch (e) {
          console.warn("[sync.bootstrap] session_from_proto_failed", s.id, e);
          diag("sync.session_from_proto_failed", { error: String(e), sid: s.id });
        }
      }
      setRootStore("sessions", rec);
      setSessionsHydrated(true);
    }
    if (workspaces.status === "fulfilled") {
      const rec: Record<string, Workspace> = {};
      for (const w of workspaces.value.workspaces) {
        rec[w.id] = {
          id: w.id as never, worker_fp: w.workerFp as never,
          name: w.name, folder_path: w.folderPath,
          color: w.color ?? null, position: w.position,
          version: Number(w.version),
          created_at_ms: Number(w.createdAtMs),
          updated_at_ms: Number(w.updatedAtMs),
          session_ids: w.sessionIds as never,
        };
      }
      setRootStore("workspaces", rec);
    }
    if (tasks.status === "fulfilled") {
      const rec: Record<string, Task> = {};
      for (const t of tasks.value.tasks) {
        rec[t.id] = {
          id: t.id as never, state: t.state as never,
          payload: JSON.parse(t.payloadJson),
          enqueued_at_ms: Number(t.enqueuedAtMs),
          claimed_at_ms: t.claimedAtMs !== undefined ? Number(t.claimedAtMs) : null,
          claimed_by: (t.claimedBy ?? null) as never,
          finished_at_ms: t.finishedAtMs !== undefined ? Number(t.finishedAtMs) : null,
          result: t.resultJson ? JSON.parse(t.resultJson) : null,
          completion_check: t.completionCheck ?? null,
          completion_check_last_attempt_ms: t.completionCheckLastAttemptMs !== undefined ? Number(t.completionCheckLastAttemptMs) : null,
          claim_ttl_ms: Number(t.claimTtlMs),
        };
      }
      setRootStore("tasks", rec);
    }
    if (permRules.status === "fulfilled") {
      const rec: Record<string, PermissionRule> = {};
      for (const r of permRules.value.rules) {
        rec[r.id] = {
          id: r.id as never,
          tool_pattern: r.toolPattern,
          folder_glob: r.folderGlob,
          decision: r.decision as never,
          enabled: r.enabled,
          created_at_ms: Number(r.createdAtMs),
        };
      }
      setRootStore("permission_rules", rec);
    }
    if (mcpRelays.status === "fulfilled") {
      const rec: Record<string, McpRelay> = {};
      for (const r of mcpRelays.value.relays) {
        rec[r.id] = {
          id: r.id as never, label: r.label, kind: r.kind as never,
          config: JSON.parse(r.configJson),
          created_at_ms: Number(r.createdAtMs),
        };
      }
      setRootStore("mcp_relays", rec);
    }
    if (pairRequests.status === "fulfilled") {
      const rec: Record<string, PairRequest> = {};
      for (const p of pairRequests.value.requests) {
        rec[p.ephemeralId] = {
          ephemeral_id: p.ephemeralId, label: p.label,
          created_at_ms: Number(p.createdAtMs),
        };
      }
      setRootStore("pair_requests", rec);
    }

    // Phase 1b: live pair-request updates ride the Sync stream now
    // (pairRequestDelta frames + per-connect snapshot seed) — the old 5 s
    // pairList poller is gone (perf sweep C2.4). The pairList seed above
    // just covers the sub-second window until the stream's first snapshot.

    // Open the Connect Sync server-stream: one server-streaming RPC
    // multiplexes all 8 domain buses + PTY bytes, reconnect-backfilling
    // via since_event_id.
    void _runConnectSync();
  } catch (err) {
    console.error("[sync] bootstrap failed", err);
    signal("diag.corruption_signal", { kind: "bootstrap_failed", msg: String(err), cooldownKey: "bootstrap" });
  }
}
