// Connect-RPC auth interceptor + caller context value. Verifies JWT from
// Authorization: Bearer <jwt>; sets caller on the context; authed
// procedures throw Code.Unauthenticated if !caller. Also writes the
// audit_log row for every Connect RPC — the interceptor is the only
// layer with both the verified caller fp AND the response status, so
// audit lives here instead of the outer fetch wrapper.
//
// JWT verification reuses apps/coord/src/jwt.ts::verifyJwt.

import { Code, ConnectError, createContextKey, type Interceptor } from "@connectrpc/connect";
import type { KyselyDB } from "../db/connection.ts";
import type { JwtCache } from "../jwt.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { CoordinatorMoveService } from "../coord-move/orchestrator.ts";
import { verifyJwt } from "../jwt.ts";
import { writeAuditLog } from "../middleware/security.ts";
import { signal, diag } from "@roost/shared/diag";

// Map a Connect Code → conventional HTTP status. Audit_log records
// HTTP-semantic status so dashboards reading `WHERE status >= 400`
// stay correct across the audit-write split (interceptor for Connect,
// coord-factory for non-Connect).
function codeToHttpStatus(code: Code): number {
  switch (code) {
    case Code.InvalidArgument:
    case Code.OutOfRange: return 400;
    case Code.Unauthenticated: return 401;
    case Code.PermissionDenied: return 403;
    case Code.NotFound: return 404;
    case Code.AlreadyExists:
    case Code.Aborted: return 409;
    case Code.FailedPrecondition: return 412;
    case Code.ResourceExhausted: return 429;
    case Code.Unimplemented: return 501;
    case Code.Unavailable: return 503;
    case Code.DeadlineExceeded: return 504;
    default: return 500;
  }
}

export interface Caller {
  fingerprint: string;
  label: string;
}

// Context-key for the caller. Handlers retrieve via `ctx.values.get(callerKey)`.
const callerKey = createContextKey<Caller | null>(null);

// trace_id context key (propagated from x-roost-trace-id header).
const traceIdKey = createContextKey<string | undefined>(undefined);

// remoteAddress context key (set via x-roost-remote-addr by bun-handler).
export const remoteAddressKey = createContextKey<string | undefined>(undefined);

// Per-tab id (set via x-roost-tab-id header by SPA's connect interceptor).
// Disambiguates multiple tabs from the same browser fingerprint so the
// viewport-claim maps (coord _viewersBySession + worker viewportClaims)
// don't collapse two tabs into one entry. See `sessionsResize` handler.
export const tabIdKey = createContextKey<string | undefined>(undefined);

export interface AuthInterceptorDeps {
  db: KyselyDB;
  jwtCache: JwtCache;
  cfg: CoordConfig;
  move?: CoordinatorMoveService;
}

// Only durable coordinator mutations take a gate lease. Read RPCs and
// server-streaming endpoints must never hold the drain open.
const WRITE_METHODS: Record<string, true | undefined> = {
  WorkersRegister: true, WorkersHeartbeat: true, WorkersRename: true, WorkersDelete: true, WorkersDeployStart: true,
  SessionsSpawn: true, SessionsAttach: true, SessionsKill: true, SessionsRename: true, SessionsResize: true,
  SessionsUserMessage: true, SessionsInput: true, SessionsCursorPos: true, SessionsAssignWorkspace: true,
  WorkspacesCreate: true, WorkspacesUpdate: true, WorkspacesDelete: true, WorkspacesSetSessions: true,
  TasksEnqueue: true, TasksNextPending: true, TasksSetState: true, TasksCancel: true,
  WebhookTokensMint: true, WebhookTokensDelete: true,
  PermissionsCreate: true, PermissionsUpdate: true, PermissionsDelete: true,
  McpCreate: true, McpDelete: true, McpPublish: true,
  AuthAuthorizeBrowser: true, AuthMintBootstrap: true, AuthRedeemWorker: true, AuthRedeemBrowser: true,
  PairCreate: true, PairApprove: true, PairDeny: true,
  FilesMkdir: true, TranscriptionSetConfig: true, AgentConfigSet: true,
  PushSubscribe: true, PushUnsubscribe: true, AttachFileChunk: true, DeleteAttachment: true,
  TransfersStart: true, DiagDebugLogBatch: true,
};

export function makeAuthInterceptor(deps: AuthInterceptorDeps): Interceptor {
  return (next) => async (req) => {
    let caller: Caller | null = null;
    // Per-RPC record lives in audit_log (finally below) — method/path/
    // status/trace_id/caller_fp; the OTel span wrapper was retired
    // (PONYTAIL-BORDERLINE B1, no exporter was ever configured).
    const method = req.method?.name ?? "unknown";
    const service = req.service?.typeName ?? "unknown";
    const path = `/${service}/${method}`;
    const auth = req.header.get("authorization");
    if (auth?.startsWith("Bearer ")) {
      const token = auth.slice(7);
      try {
        const c = await verifyJwt(token, {
          db: deps.db,
          cache: deps.jwtCache,
          jwtMaxAgeSecs: deps.cfg.jwtMaxAgeSecs,
        });
        caller = { fingerprint: c.fingerprint, label: c.label };
      } catch {
        diag("auth.jwt_verify_failed", { path });
        // leave caller null; authed handlers reject
      }
    }
    const traceId = req.header.get("x-roost-trace-id") ?? undefined;
    req.contextValues.set(callerKey, caller);
    req.contextValues.set(traceIdKey, traceId);
    req.contextValues.set(remoteAddressKey, req.header.get("x-roost-remote-addr") ?? undefined);
    req.contextValues.set(tabIdKey, req.header.get("x-roost-tab-id") ?? undefined);
    let status = 200;
    const lease = deps.move && WRITE_METHODS[method] ? deps.move.gate.acquire() : null;
    try {
      return await next(req);
    } catch (e) {
      status = e instanceof ConnectError ? codeToHttpStatus(e.code) : 500;
      throw e;
    } finally {
      lease?.release();
      writeAuditLog({
        db: deps.db, status, method: "POST", path, traceId,
        callerFp: caller?.fingerprint ?? null,
      });
      if (status === 401) {
        signal("auth.rpc_rejected", {
          path,
          reason: caller ? "forbidden" : "no_caller",
          caller_fp: caller?.fingerprint ?? null,
          cooldownKey: caller?.fingerprint ?? "anon",
        });
      }
    }
  };
}

/** Throws Code.Unauthenticated if no caller; returns Caller otherwise. */
export function requireAuth(values: { get<T>(k: ReturnType<typeof createContextKey<T>>): T }): Caller {
  const caller = values.get(callerKey);
  if (!caller) {
    throw new ConnectError("authentication required", Code.Unauthenticated);
  }
  return caller;
}

/** Caller if a valid JWT was presented, else null — for handlers with an
 *  alternate trust path (pairList/pairApprove/pairDeny accept a LOOPBACK
 *  caller so the on-host agent/CLI can approve devices via API — Author
 *  2026-07-11 "approve new devices via API"; see handlers-auth.ts). */
export function optionalAuth(values: { get<T>(k: ReturnType<typeof createContextKey<T>>): T }): Caller | null {
  return values.get(callerKey);
}
