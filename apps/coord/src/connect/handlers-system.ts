// System / diagnostics RPC handlers: health, db-export URL, metrics, the
// SPA diag-log batch sink, on-demand state snapshot, and the audit-log
// query. Spread into router.ts's single router.service() literal.
// Split out of router.ts (400-line cap).

import type { ServiceImpl } from "@connectrpc/connect";
import { create } from "@bufbuild/protobuf";
import { log } from "@roost/shared/log";
import { isDiagEnabled } from "@roost/shared/diag";
import {
  CoordinatorService,
  MiscHealthResponseSchema,
  MiscDbExportUrlResponseSchema, MiscMetricsResponseSchema,
  DiagDebugLogBatchResponseSchema, DiagSnapshotResponseSchema,
  AuditListResponseSchema,
} from "@roost/shared/proto/coordinator_pb";
import { AuditRowSchema } from "@roost/shared/proto/wire_pb";
import { callerOrigin, requireAuth } from "./auth-interceptor.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { ROOST_ARTIFACT_VERSION } from "@roost/shared/build-identity";
import { getMetricsSnapshot } from "../telemetry.ts";
import { _viewersBySession } from "./viewer-tracker.ts";
import {
  _cellSubscriptionSnapshot,
  _cellSubscriptionStateSnapshot,
  type CellSubscriptionDiagnostic,
} from "./cell-subscriptions.ts";
import {
  _announcedBarrierSnapshot,
  _barrierRepairSnapshot,
  _lastCellSnapshot,
  _sessionRouteSnapshot,
  type CoordinatorBarrierRepairDiagnostic,
  type CoordinatorLastCellDiagnostic,
} from "../byte-hub.ts";
import { getAgentStatusDiagnostics } from "../agent-status-hub.ts";
import { connectWorkers } from "./worker-registry.ts";
import { collectWorkerDiagSnapshots } from "./worker-send.ts";
import { _pendingRpcStats } from "../router/pending-rpcs.ts";
import type { ConnectDeps } from "./router.ts";

// Coord process boot time — captured at module load (coord startup). Used
// by miscHealth for uptime.
const BOOT_MS = Date.now();

type SystemMethods =
  | "miscHealth" | "miscDbExportUrl" | "miscMetrics"
  | "diagDebugLogBatch" | "diagSnapshot" | "auditList";

export function makeSystemHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SystemMethods> {
  return {
    // ─── misc ──────────────────────────────────────────────────────────
    async miscHealth(_req, _ctx) {
      // public
      return create(MiscHealthResponseSchema, {
        ok: true, bootMs: BigInt(BOOT_MS),
        uptimeMs: BigInt(Date.now() - BOOT_MS), gitSha: COORD_GIT_SHA,
      });
    },

    async miscDbExportUrl(_req, ctx) {
      requireAuth(ctx.values);
      assertOnHost(callerOrigin(ctx.values));
      const port = deps.cfg.bind.split(":").pop();
      return create(MiscDbExportUrlResponseSchema, {
        url: `http://127.0.0.1:${port}/api/db-export`,
      });
    },

    async miscMetrics(_req, ctx) {
      requireAuth(ctx.values);
      const m = getMetricsSnapshot();
      const requests: Record<string, bigint> = {};
      const errors: Record<string, bigint> = {};
      for (const [k, v] of Object.entries(m.requests)) requests[k] = BigInt(v);
      for (const [k, v] of Object.entries(m.errors)) errors[k] = BigInt(v);
      return create(MiscMetricsResponseSchema, {
        uptimeMs: BigInt(m.uptime_ms),
        requests, errors,
        totalRequests: BigInt(m.total_requests),
        totalErrors: BigInt(m.total_errors),
      });
    },

    // ─── diag (terminal-corruption diagnostics) ──────────────────────
    // SPA batches diag events client-side and uploads them every 100ms
    // or 64 entries. Coord re-emits each entry via the log facade so
    // they land in RoostCoord/main.out.log with the canonical JSON shape.
    // Target="diag" so grep stays independent of operational logs.
    async diagDebugLogBatch(req, ctx) {
      requireAuth(ctx.values);
      // Tier-1 signals always land. The diag firehose (info entries) is
      // dropped unless coord-side ROOST_DIAG=1 — a single coord switch
      // governs disk/CPU even when a stale browser keeps localStorage.roostDiag=1
      // and uploads. ponytail: gate, don't chase every browser's localStorage.
      const diagFirehoseOn = isDiagEnabled();
      let accepted = 0;
      for (const e of req.entries) {
        if (!e.signal && !diagFirehoseOn) continue;
        const kv: Record<string, unknown> = {};
        if (e.kvJson) {
          try {
            const parsed = JSON.parse(e.kvJson);
            if (parsed && typeof parsed === "object") Object.assign(kv, parsed);
          } catch { /* drop malformed */ }
        }
        const fields = {
          // Explicit evt so the kind survives even when kv carries its own
          // `msg` (e.g. spa.uncaught's error text), which would otherwise
          // clobber the structural log msg. `roost doctor` groups by evt.
          evt: e.evt,
          ts_spa: Number(e.tsMs),
          mono_ns: Number(e.monoNs),
          trace_id: e.traceId || undefined,
          session_trace_id: e.sessionTraceId || undefined,
          sid: e.sid || undefined,
          viewer_key: e.viewerKey || undefined,
          src: "spa",
          ...kv,
        };
        // Tier-1 signals → log.warn(target="signal") so they land in
        // *.err.log (the always-on daily-review channel `roost doctor`
        // reads). Firehose diag entries → log.info(target="diag") (*.out.log).
        if (e.signal) log.warn("signal", e.evt, fields);
        else log.info("diag", e.evt, fields);
        accepted++;
      }
      return create(DiagDebugLogBatchResponseSchema, { accepted });
    },

    // On-demand state dump. The coordinator captures its bounded local state,
    // requests one correlated snapshot from every known worker that is live,
    // and waits only through the worker-RPC deadline before returning.
    async diagSnapshot(req, ctx) {
      requireAuth(ctx.values);
      const capturedAtMs = Date.now();
      const connectedWorkerFps = new Set(connectWorkers.keys());
      const routeState = _sessionRouteSnapshot();
      const lastCellState = _lastCellSnapshot();
      const barrierRepairState = _barrierRepairSnapshot(capturedAtMs);
      const subscriptionState = _cellSubscriptionStateSnapshot(capturedAtMs);
      const activeSubscriptions = _cellSubscriptionSnapshot();
      const openSessions = await deps.db.selectFrom("sessions")
        .select(["id", "worker_fp", "channel"])
        .where("status", "=", "open")
        .execute();
      const registeredWorkers = await deps.db.selectFrom("workers")
        .select("fp")
        .execute();

      type CoordSessionDiagnostic = {
        route: {
          worker_fp: string;
          channel_id: number;
          connected: boolean;
          source: "live_cache" | "database";
        } | null;
        last_cell: CoordinatorLastCellDiagnostic | null;
        /** Routes whose announced cell frames were dropped: while a mark stands,
         *  the coordinator forces that route's next claim to fetch a full frame. */
        barrier_repair: CoordinatorBarrierRepairDiagnostic[];
        viewers: Record<string, {
          cols: number;
          rows: number;
          last_ms: number;
          age_ms: number;
          client_seq: number;
        }>;
        subscriptions: Record<string, CellSubscriptionDiagnostic>;
      };
      const sessions: Record<string, CoordSessionDiagnostic> = {};
      const ensureSession = (sessionId: string): CoordSessionDiagnostic => {
        let state = sessions[sessionId];
        if (!state) {
          const route = routeState[sessionId];
          state = {
            route: route ? {
              ...route,
              connected: connectedWorkerFps.has(route.worker_fp),
              source: "live_cache",
            } : null,
            last_cell: lastCellState[sessionId] ?? null,
            barrier_repair: barrierRepairState[sessionId] ?? [],
            viewers: {},
            subscriptions: {},
          };
          sessions[sessionId] = state;
        }
        return state;
      };

      for (const row of openSessions) {
        const state = ensureSession(row.id);
        if (!state.route) {
          state.route = {
            worker_fp: row.worker_fp,
            channel_id: row.channel,
            connected: connectedWorkerFps.has(row.worker_fp),
            source: "database",
          };
        }
      }
      for (const [sessionId, viewers] of _viewersBySession) {
        const state = ensureSession(sessionId);
        for (const [viewerId, viewer] of viewers) {
          state.viewers[viewerId] = {
            cols: viewer.cols,
            rows: viewer.rows,
            last_ms: viewer.lastMs,
            age_ms: Math.max(0, capturedAtMs - viewer.lastMs),
            client_seq: viewer.clientSeq,
          };
        }
      }
      for (const [viewerId, bySession] of Object.entries(subscriptionState)) {
        for (const [sessionId, subscription] of Object.entries(bySession)) {
          ensureSession(sessionId).subscriptions[viewerId] = subscription;
        }
      }
      for (const sessionId of Object.keys(lastCellState)) ensureSession(sessionId);
      for (const sessionId of Object.keys(barrierRepairState)) ensureSession(sessionId);

      const workerFps = new Set(registeredWorkers.map((worker) => worker.fp));
      for (const workerFp of connectedWorkerFps) workerFps.add(workerFp);
      const workers = await collectWorkerDiagSnapshots(workerFps);
      const coordState: Record<string, unknown> = {
        build: {
          git_sha: COORD_GIT_SHA,
          artifact_version: ROOST_ARTIFACT_VERSION,
        },
        sessions,
        cell_subscriptions: activeSubscriptions,
        agent_status: getAgentStatusDiagnostics(),
        announced_barrier: _announcedBarrierSnapshot(),
        terminal_control: {
          pending_rpcs: _pendingRpcStats().pending,
        },
      };

      let spaPayload: unknown = null;
      if (req.spaStateJson) {
        try { spaPayload = JSON.parse(req.spaStateJson); } catch { /* explicit null below */ }
      }

      const snapshot = {
        captured_at_ms: capturedAtMs,
        coord: coordState,
        workers,
        spa: spaPayload,
      };
      const snapshotJson = JSON.stringify(snapshot);
      log.info("diag", "diag.snapshot", { src: "coord", snapshot_size: snapshotJson.length });
      return create(DiagSnapshotResponseSchema, { snapshotJson });
    },

    // ─── audit ────────────────────────────────────────────────────────
    async auditList(req, ctx) {
      requireAuth(ctx.values);
      const limit = Math.min(req.limit || 100, 500);
      let q = deps.db.selectFrom("audit_log as a")
        .leftJoin("authorized_keys as k", "k.fingerprint", "a.caller_fp")
        .select(["a.id", "a.ts", "a.caller_fp", "k.label as caller_label",
                 "a.method", "a.path", "a.status", "a.trace_id"])
        .orderBy("a.id", "desc").limit(limit + 1);
      if (req.cursor) q = q.where("a.id", "<", parseInt(req.cursor, 10));
      if (req.callerFp) q = q.where("a.caller_fp", "=", req.callerFp);
      if (req.method) q = q.where("a.method", "=", req.method);
      const raw = await q.execute();
      const hasMore = raw.length > limit;
      const rows = (hasMore ? raw.slice(0, limit) : raw).map(r => create(AuditRowSchema, {
        id: BigInt(r.id as number),
        ts: BigInt(r.ts as number),
        callerFp: (r.caller_fp as string | null) ?? undefined,
        callerLabel: (r.caller_label as string | null) ?? undefined,
        method: r.method as string,
        path: r.path as string,
        status: r.status as number,
        traceId: (r.trace_id as string | null) ?? undefined,
      }));
      const lastRow = rows[rows.length - 1];
      const next_cursor = hasMore && lastRow ? String(lastRow.id) : undefined;
      return create(AuditListResponseSchema, { rows, nextCursor: next_cursor });
    },
  };
}
