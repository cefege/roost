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
} from "./auth-interceptor.ts";
import { assertOnHost } from "../middleware/caller-origin.ts";
import { COORD_GIT_SHA } from "../git-sha.ts";
import { ROOST_ARTIFACT_VERSION } from "@roost/shared/build-identity";
import { getMetricsSnapshot } from "../telemetry.ts";
import {
  coordSessionDiagnostic,
  type CoordSessionDiagnostic,
} from "./diag-snapshot-session-state.ts";
import { connectWorkers } from "./worker-registry.ts";
import type { ConnectDeps } from "./router.ts";
import { createScopedWorkerDiagnosticCollector } from "./diag-snapshot-worker-results.ts";
import { createTerminalCaptureBridge } from "./terminal-capture.ts";

// Coord process boot time — captured at module load (coord startup). Used
// by miscHealth for uptime.
const BOOT_MS = Date.now();
const DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS = 64;

function normalizeDiagSnapshotSessionFilterIds(
  sessionFilterId: string,
  sessionFilterIds: readonly string[],
): string[] {
  if (sessionFilterId !== "" && sessionFilterIds.length !== 0) {
    throw new ConnectError(
      "diag snapshot session_filter_id and session_filter_ids cannot be combined",
      Code.InvalidArgument,
    );
  }
  if (sessionFilterIds.length > DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS) {
    throw new ConnectError(
      "diag snapshot accepts at most 64 session_filter_ids",
      Code.InvalidArgument,
    );
  }
  const normalizedSessionFilterIds = sessionFilterId === ""
    ? [...sessionFilterIds]
    : [sessionFilterId];
  if (
    normalizedSessionFilterIds.some((sessionId) => sessionId === "")
    || new Set(normalizedSessionFilterIds).size !== normalizedSessionFilterIds.length
  ) {
    throw new ConnectError(
      "diag snapshot session_filter_ids must be unique and nonempty",
      Code.InvalidArgument,
    );
  }
  return normalizedSessionFilterIds;
}

/** A capture request addresses exactly ONE session, named twice: the RPC's own
 *  resource scope and the capture command must agree, and the legacy scalar
 *  filter cannot express that agreement. */
function assertTerminalCaptureSessionFilter(
  sessionFilterId: string,
  sessionFilterIds: readonly string[],
  captureSessionId: string,
): void {
  if (sessionFilterId !== "") {
    throw new ConnectError(
      "terminal capture requires session_filter_ids, not session_filter_id",
      Code.InvalidArgument,
    );
  }
  if (sessionFilterIds.length !== 1 || sessionFilterIds[0] !== captureSessionId) {
    throw new ConnectError(
      "terminal capture requires exactly one session_filter_ids entry matching terminal_capture.session_id",
      Code.InvalidArgument,
    );
  }
}


type SystemMethods =
  | "miscHealth" | "miscDbExportUrl" | "miscMetrics"
  | "diagDebugLogBatch" | "diagSnapshot" | "auditList";

export function makeSystemHandlers(
  deps: ConnectDeps,
): Pick<ServiceImpl<typeof CoordinatorService>, SystemMethods> {
  const collectScopedWorkerDiagnostics = createScopedWorkerDiagnosticCollector();
  const captureBridge = createTerminalCaptureBridge(deps);
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
      requireAccountDevice(ctx.values);
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

    // On-demand state dump. A session-filtered diagnosis narrows to selected
    // sessions; an unfiltered dump covers the whole fleet. A terminal_capture
    // request is neither: it is one authenticated capture step on exactly one
    // session, answered with the capture result and nothing else.
    async diagSnapshot(req, ctx) {
      const caller = requireAccountDevice(ctx.values);
      const captureRequest = req.terminalCapture;
      if (captureRequest !== undefined) {
        assertTerminalCaptureSessionFilter(
          req.sessionFilterId,
          req.sessionFilterIds ?? [],
          captureRequest.sessionId,
        );
        const terminalCapture = await captureBridge.handle(captureRequest, caller);
        return create(DiagSnapshotResponseSchema, {
          snapshotJson: JSON.stringify({
            captured_at_ms: Date.now(),
            terminal_capture: terminalCapture,
          }),
        });
      }
      const requestedSessionFilterIds = req.sessionFilterIds ?? [];
      const sessionFilterIds = normalizeDiagSnapshotSessionFilterIds(
        req.sessionFilterId,
        requestedSessionFilterIds,
      );
      const filtered = sessionFilterIds.length !== 0;
      const capturedAtMs = Date.now();

      // Resolve every resource boundary from durable session and worker rows
      // before touching coordinator caches. A filtered diagnostic only looks up
      // the selected sessions' workers, not the whole fleet.
      let sessionQuery = deps.db.selectFrom("sessions as session")
        .innerJoin("workers as worker", "worker.fp", "session.worker_fp")
        .select([
          "session.id as id",
          "session.worker_fp as worker_fp",
          "session.channel as channel",
        ])
        .where("session.status", "=", "open")
        .where("worker.deleted_at_ms", "is", null);
      if (filtered) {
        sessionQuery = sessionQuery.where("session.id", "in", sessionFilterIds);
      } else {
        // An unfiltered dump covers the whole fleet; cap it at the same bound
        // the filtered path enforces so the snapshot cannot grow with the DB.
        sessionQuery = sessionQuery.limit(DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS);
      }
      const scopedSessionRows = await sessionQuery.execute();
      const sessionWorkerFps = [...new Set(scopedSessionRows.map((row) => row.worker_fp))];
      const scopedWorkerRows = filtered
        ? sessionWorkerFps.length === 0
          ? []
          : await deps.db.selectFrom("workers")
            .select("fp")
            .where("fp", "in", sessionWorkerFps)
            .where("deleted_at_ms", "is", null)
            .execute()
        : await deps.db.selectFrom("workers")
          .select("fp")
          .where("deleted_at_ms", "is", null)
          .limit(DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS)
          .execute();
      const truncated = !filtered
        && (scopedSessionRows.length === DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS
          || scopedWorkerRows.length === DIAG_SNAPSHOT_MAX_SESSION_FILTER_IDS);
      const allowedSessionIds = new Set(scopedSessionRows.map((row) => row.id));
      // The capped worker page can miss a worker that owns an admitted session;
      // its live, non-deleted row is already proven by the session join, so
      // seeding it keeps a truncated dump from misreporting route: null.
      const allowedWorkerFps = new Set([
        ...scopedWorkerRows.map((row) => row.fp),
        ...sessionWorkerFps,
      ]);

      // The registry is volatile, so a route, a connection bit, or a worker
      // dispatch is only taken for a worker the durable predicate admitted.
      const dispatchableWorkerFps = new Set<string>();
      for (const workerFp of allowedWorkerFps) {
        const handle = connectWorkers.get(workerFp);
        if (
          handle !== undefined
          && handle.ready
          && !handle.revoked
        ) {
          dispatchableWorkerFps.add(workerFp);
        }
      }

      const sessions: Record<string, CoordSessionDiagnostic> = {};
      for (const row of scopedSessionRows) {
        sessions[row.id] = coordSessionDiagnostic(row, {
          allowedWorkerFps,
          dispatchableWorkerFps,
        });
      }

      const workers = await collectScopedWorkerDiagnostics({
        workerFps: dispatchableWorkerFps,
        sessions: scopedSessionRows,
        allowedSessionIds,
      });
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
        ...(truncated ? { truncated: true } : {}),
      };
      const snapshotJson = JSON.stringify(snapshot);
      log.info("diag", "diag.snapshot", { src: "coord", snapshot_size: snapshotJson.length });
      return create(DiagSnapshotResponseSchema, { snapshotJson });
    },

    // ─── audit ────────────────────────────────────────────────────────
    async auditList(req, ctx) {
      requireAccountDevice(ctx.values);
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
