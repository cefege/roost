// System / diagnostics RPC handlers: health, db-export URL, metrics, the
// SPA diag-log batch sink, on-demand state snapshot, and the audit-log
// query. Spread into router.ts's single router.service() literal.
// Split out of router.ts (400-line cap).

import { Code, ConnectError, type ServiceImpl } from "@connectrpc/connect";
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
import {
  callerOrigin,
  requireAccountDevice,
  requireDashboardActor,
  requireDashboardAdmin,
} from "./auth-interceptor.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { ROOST_ARTIFACT_VERSION } from "@roost/shared/build-identity";
import { getMetricsSnapshot } from "../telemetry.ts";
import {
  currentTerminalScreenHub,
  terminalViewSnapshot,
} from "./terminal-view-hub.ts";
import { getCachedSessionWorker } from "../byte-hub.ts";
import { connectWorkers } from "./worker-registry.ts";
import {
  collectWorkerDiagSnapshots,
  type WorkerDiagSnapshotResult,
} from "./worker-send.ts";
import type { ConnectDeps } from "./router.ts";

// Coord process boot time — captured at module load (coord startup). Used
// by miscHealth for uptime.
const BOOT_MS = Date.now();

/**
 * A worker can retain stale sessions while it is being reassigned. Keep only
 * the actor-authorized session records; process-wide worker counters would
 * otherwise disclose activity outside the selected dashboard.
 */
function scopedWorkerDiagnostic(
  workerFp: string,
  result: WorkerDiagSnapshotResult,
  allowedSessionIds: ReadonlySet<string>,
): WorkerDiagSnapshotResult {
  if (result.status !== "ok") return result;
  const sourceSessions = result.snapshot.sessions;
  const sessions = sourceSessions !== null
    && typeof sourceSessions === "object"
    && !Array.isArray(sourceSessions)
    ? Object.fromEntries(
      Object.entries(sourceSessions)
        .filter(([sessionId]) => allowedSessionIds.has(sessionId)),
    )
    : {};
  return {
    ...result,
    snapshot: {
      captured_at_ms: result.snapshot.captured_at_ms,
      build: result.snapshot.build,
      worker_fp: workerFp,
      sessions,
    },
  };
}

type SystemMethods =
  | "miscHealth" | "miscDbExportUrl" | "miscMetrics"
  | "diagDebugLogBatch" | "diagSnapshot" | "auditList";

export function makeSystemHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SystemMethods> {
  return {
    // ─── misc ──────────────────────────────────────────────────────────
    async miscHealth(_req, ctx) {
      // Self-hosted health remains public. Managed health is a browser RPC and
      // must cross the same verified device boundary as the rest of the app.
      if (deps.cfg.saasMode) requireAccountDevice(ctx.values);
      return create(MiscHealthResponseSchema, {
        ok: true, bootMs: BigInt(BOOT_MS),
        uptimeMs: BigInt(Date.now() - BOOT_MS), gitSha: COORD_GIT_SHA,
      });
    },

    async miscDbExportUrl(_req, ctx) {
      if (deps.cfg.saasMode) {
        throw new ConnectError("database export is unavailable in managed mode", Code.PermissionDenied);
      }
      requireAccountDevice(ctx.values);
      assertOnHost(callerOrigin(ctx.values));
      const port = deps.cfg.bind.split(":").pop();
      return create(MiscDbExportUrlResponseSchema, {
        url: `http://127.0.0.1:${port}/api/db-export`,
      });
    },

    async miscMetrics(_req, ctx) {
      requireAccountDevice(ctx.values);
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
      requireDashboardActor(ctx.values);
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

    // On-demand state dump. A filtered session diagnosis is available to
    // dashboard members; an unfiltered fleet dump is an admin diagnostic.
    async diagSnapshot(req, ctx) {
      const sessionFilterId: string = req.sessionFilterId || "";
      const actor = sessionFilterId === ""
        ? requireDashboardAdmin(ctx.values)
        : requireDashboardActor(ctx.values);
      const capturedAtMs = Date.now();

      // Resolve every resource boundary from durable dashboard predicates
      // before touching coordinator caches. The filtered attach poller only
      // looks up its one session's worker, not the dashboard's whole fleet.
      let sessionQuery = deps.db.selectFrom("sessions as session")
        .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
        .select([
          "session.id as id",
          "session.worker_fp as worker_fp",
          "session.channel as channel",
        ])
        .where("session.dashboard_id", "=", actor.dashboardId)
        .where("session.status", "=", "open")
        .where("worker.dashboard_id", "=", actor.dashboardId)
        .where("worker.deleted_at_ms", "is", null);
      if (sessionFilterId !== "") {
        sessionQuery = sessionQuery.where("session.id", "=", sessionFilterId);
      }
      const scopedSessionRows = await sessionQuery.execute();
      const sessionWorkerFps = [...new Set(scopedSessionRows.map((row) => row.worker_fp))];
      const scopedWorkerRows = sessionFilterId !== ""
        ? sessionWorkerFps.length === 0
          ? []
          : await deps.db.selectFrom("workers")
            .select("fp")
            .where("dashboard_id", "=", actor.dashboardId)
            .where("fp", "in", sessionWorkerFps)
            .where("deleted_at_ms", "is", null)
            .execute()
        : await deps.db.selectFrom("workers")
          .select("fp")
          .where("dashboard_id", "=", actor.dashboardId)
          .where("deleted_at_ms", "is", null)
          .execute();
      const allowedSessionIds = new Set(scopedSessionRows.map((row) => row.id));
      const allowedWorkerFps = new Set(scopedWorkerRows.map((row) => row.fp));
      const workerFpsToDiagnose = new Set(
        sessionFilterId === ""
          ? allowedWorkerFps
          : sessionWorkerFps.filter((workerFp) => allowedWorkerFps.has(workerFp)),
      );

      // The registry is volatile, so its server-stamped dashboard scope must
      // agree with the durable predicate before it is used for a route, a
      // connection bit, or a worker dispatch.
      const dispatchableWorkerFps = new Set<string>();
      for (const workerFp of workerFpsToDiagnose) {
        const handle = connectWorkers.get(workerFp);
        if (
          handle !== undefined
          && handle.dashboardId === actor.dashboardId
          && handle.ready
          && !handle.revoked
        ) {
          dispatchableWorkerFps.add(workerFp);
        }
      }

      type CoordSessionDiagnostic = {
        route: {
          worker_fp: string;
          channel_id: number;
          connected: boolean;
          source: "live_cache" | "database";
        } | null;
        terminal_view: {
          activeViews: number;
          parkedViews: number;
          streamId: string;
          effective: { cols: number; rows: number } | null;
          unavailable: boolean;
        } | null;
        terminal_screen: {
          stream_id: string;
          grid_epoch: string;
          seq: string;
          cols: number;
          rows: number;
          valid: boolean;
        } | null;
        viewers: Record<string, { cols: number; rows: number }>;
      };
      const terminalScreen = currentTerminalScreenHub();
      const sessions: Record<string, CoordSessionDiagnostic> = {};
      for (const row of scopedSessionRows) {
        const cachedRoute = getCachedSessionWorker(row.id);
        const state: CoordSessionDiagnostic = {
          route: cachedRoute && allowedWorkerFps.has(cachedRoute.worker_fp)
            ? {
              worker_fp: cachedRoute.worker_fp,
              channel_id: cachedRoute.channel,
              connected: dispatchableWorkerFps.has(cachedRoute.worker_fp),
              source: "live_cache",
            }
            : allowedWorkerFps.has(row.worker_fp)
              ? {
                worker_fp: row.worker_fp,
                channel_id: row.channel,
                connected: dispatchableWorkerFps.has(row.worker_fp),
                source: "database",
              }
              : null,
          terminal_view: terminalViewSnapshot(row.id),
          terminal_screen: null,
          // Viewer projection only has a process-wide snapshot API. Do not
          // enumerate it for a tenant diagnostic; terminal_view has scoped
          // aggregate view state above.
          viewers: {},
        };
        const screen = terminalScreen?.snapshot(row.id);
        if (screen) {
          state.terminal_screen = {
            stream_id: screen.streamId,
            grid_epoch: screen.gridEpoch,
            seq: screen.seq.toString(),
            cols: screen.cols,
            rows: screen.rows,
            valid: screen.valid,
          };
        }
        sessions[row.id] = state;
      }

      const rawWorkers = await collectWorkerDiagSnapshots(
        dispatchableWorkerFps,
        undefined,
        actor.dashboardId,
      );
      const workers = Object.fromEntries(
        Object.entries(rawWorkers).map(([workerFp, result]) => [
          workerFp,
          scopedWorkerDiagnostic(workerFp, result, allowedSessionIds),
        ] as const),
      );

      const coordState: Record<string, unknown> = {
        build: {
          git_sha: COORD_GIT_SHA,
          artifact_version: ROOST_ARTIFACT_VERSION,
        },
        sessions,
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
      const actor = requireDashboardAdmin(ctx.values);
      const limit = Math.min(req.limit || 100, 500);
      let q = deps.db.selectFrom("audit_log as a")
        .leftJoin("authorized_keys as k", "k.fingerprint", "a.caller_fp")
        .select(["a.id", "a.ts", "a.caller_fp", "k.label as caller_label",
                 "a.method", "a.path", "a.status", "a.trace_id"])
        .where("a.dashboard_id", "=", actor.dashboardId)
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
