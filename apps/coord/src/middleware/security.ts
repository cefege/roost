// Security headers + CORS + audit log writes. Plain fetch-handler
// helpers — no H3 dependency.

import { DEFAULT_WORKER_LOCAL_UI_ORIGIN, type CoordConfig } from "@roost/host/config";
// Exposed-to-JS header names are part of the SPA↔coord trust contract.
import { X_ROOST_AUTH_LAYER } from "@roost/protocol/wire/headers";
import { TRACE_HEADER } from "@roost/observability/trace";
import { auditBus } from "../events/buses.ts";
import { recordRequest, recordError } from "../diagnostics/telemetry.ts";
import { signal } from "@roost/observability/diag";
import type { KyselyDB } from "../db/connection.ts";
import type { ListenerTrust } from "./caller-origin.ts";
import { applySecurityHeaders } from "@roost/host/http-security";

export interface SecurityOptions {
  relaxedCsp: boolean;
  corsAllowedOrigins: string[];
  hsts: boolean;
  connectOrigins: string[];
}

/** The worker's loopback door is a first-class browser origin in both
 * directions: its SPA's fetches carry a bearer JWT and no cookies, so allowing
 * it for CORS grants reachability only, and a page this coordinator served
 * dials that door's terminal socket, so it must be in connect-src too. */
export function securityOptionsForConfig(cfg: CoordConfig, hsts: boolean): SecurityOptions {
  const corsAllowedOrigins = [DEFAULT_WORKER_LOCAL_UI_ORIGIN, ...cfg.corsAllowedOrigins];
  const origins = new Set<string>([
    "https://api.deepgram.com",
    "wss://api.deepgram.com",
  ]);
  for (
    const raw of [
      cfg.publicUrl,
      cfg.webPublicUrl,
      DEFAULT_WORKER_LOCAL_UI_ORIGIN,
      ...cfg.corsAllowedOrigins,
    ]
  ) {
    if (!raw) continue;
    const origin = new URL(raw).origin;
    origins.add(origin);
    // Every browser transport this page opens has a WebSocket twin: Sync to the
    // coordinator, and the local terminal socket to a worker's loopback door.
    // The door is plaintext, so the twin must be derived from either scheme.
    origins.add(origin.replace(/^http/, "ws"));
  }
  return {
    relaxedCsp: cfg.relaxedCsp,
    corsAllowedOrigins,
    hsts,
    connectOrigins: [...origins],
  };
}

export function applyCors(
  headers: Headers,
  reqOrigin: string | null,
  allowedOrigins: string[],
): void {
  if (reqOrigin && allowedOrigins.includes(reqOrigin)) {
    headers.set("access-control-allow-origin", reqOrigin);
    headers.set("access-control-expose-headers", X_ROOST_AUTH_LAYER);
  }
  headers.set("vary", "origin, access-control-request-method, access-control-request-headers");
  headers.set("access-control-allow-methods", "*");
  headers.set("access-control-allow-headers", "*");
}

export function wrapResponse(
  resp: Response,
  req: Request,
  opts: SecurityOptions,
): Response {
  // Mutate in place: re-wrapping converts a Bun.file() body into a
  // ReadableStream, which drops content-length and installs Bun's
  // RequestContext.onAbort path (see connect/bun-handler.ts).
  applyCors(resp.headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(resp.headers, opts.relaxedCsp, opts.hsts, opts.connectOrigins);
  return resp;
}

export function preflightResponse(req: Request, opts: SecurityOptions): Response {
  const headers = new Headers();
  applyCors(headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(headers, opts.relaxedCsp, opts.hsts, opts.connectOrigins);
  return new Response(null, { status: 204, headers });
}

/** Audit metadata captured per request; passed to writeAuditLog after the response. */
export interface AuditMeta {
  method: string;
  path: string;
  traceId: string | undefined;
}

export function extractAuditMeta(req: Request): AuditMeta {
  const url = new URL(req.url);
  return {
    method: req.method,
    path: url.pathname,
    traceId: req.headers.get(TRACE_HEADER) ?? undefined,
  };
}
export const SPA_AUDIT_TELEMETRY_PATH = "<spa-static>";
export const API_NOT_FOUND_AUDIT_PATH = "<api-404>";

export type NonConnectAuditSurface = "spa" | "db-export" | "api";

/** Successful static/deep-link reads have no durable forensic value, and an
 * unmatched /api/* path is an unauthenticated GET the rate limiter lets
 * through, so one durable row per probed path is pure amplification. Every
 * other error and the explicit API/export surfaces remain auditable. */
export function shouldPersistNonConnectAudit(opts: {
  surface: NonConnectAuditSurface;
  method: string;
  status: number;
}): boolean {
  if (opts.surface === "api" && opts.status === 404) return false;
  return !(
    opts.surface === "spa"
    && (opts.method === "GET" || opts.method === "HEAD")
    && opts.status >= 200
    && opts.status < 400
  );
}

/** An anonymous credential failure that arrived through the operator's front
 * door carries no forensic value and is unsweepable: audit-retention.ts is an
 * explicit allowlist that never ages out auth rows, so an internet-facing
 * scanner would otherwise grow audit_log without bound (it once reached
 * 7,026,358 rows / 1.0 GB). Bounded telemetry and cooldown-coalesced signals
 * cover the same anomaly. An on-host caller is low volume and high signal, so
 * its 401 still persists. */
export function shouldPersistConnectAudit(opts: {
  listener: ListenerTrust;
  status: number;
  callerFp: string | null;
}): boolean {
  return !(
    opts.listener === "trusted-proxy"
    && opts.status === 401
    && opts.callerFp === null
  );
}

export function recordAuditTelemetry(path: string, status: number): void {
  recordRequest(path);
  if (status >= 400) recordError(path);
}


/** Audit metadata accepted by the durable audit writer. */
export interface AuditLogOptions {
  db: KyselyDB;
  status: number;
  method: string;
  path: string;
  traceId: string | undefined;
  callerFp: string | null;
  /** Retained scope column for the audit row; storage metadata only. */
  dashboardId?: string | null;
  /** Terminal input uses strict mode so a completed write cannot be reported
   * without an explicit audit-persistence outcome. Other request audits remain
   * best-effort to avoid changing interceptor failure semantics. */
  throwOnFailure?: boolean;
  /** Set false when the caller already recorded telemetry before applying a
   * durable-audit predicate. */
  recordTelemetry?: boolean;
}

/** Writes a same-database audit batch atomically, then publishes its committed
 * rows in durable ID order. */
export async function writeAuditLogs(entries: readonly AuditLogOptions[]): Promise<void> {
  if (entries.length === 0) return;
  const db = entries[0]!.db;
  for (const entry of entries) {
    if (entry.db !== db) throw new Error("audit batch must use one database");
  }
  let shouldThrowOnFailure = false;
  for (const entry of entries) {
    if (entry.recordTelemetry !== false) recordAuditTelemetry(entry.path, entry.status);
    if (entry.throwOnFailure) shouldThrowOnFailure = true;
  }
  const rows = entries.map((entry) => {
    const timestamp = Date.now();
    return {
      ts: timestamp,
      caller_fp: entry.callerFp,
      dashboard_id: entry.dashboardId ?? null,
      method: entry.method,
      path: entry.path,
      status: entry.status,
      trace_id: entry.traceId ?? null,
    };
  });
  try {
    const inserted = await db.transaction().execute(async (transaction) => {
      const insert = transaction
        .insertInto("audit_log")
        .values(rows)
        .returning(["id", "ts", "caller_fp", "method", "path", "status", "trace_id"]);
      return insert.execute();
    });
    inserted.sort((left, right) => {
      const leftId = Number(left.id);
      const rightId = Number(right.id);
      return leftId - rightId;
    });
    for (const row of inserted) {
      auditBus.publish({
        id: row.id as number,
        ts: row.ts as number,
        caller_fp: (row.caller_fp as string | null) ?? null,
        caller_label: null,
        method: row.method as string,
        path: row.path as string,
        status: row.status as number,
        trace_id: (row.trace_id as string | null) ?? null,
      });
    }
  } catch (error) {
    for (const entry of entries) {
      signal("audit.write_failed", {
        error: String(error),
        path: entry.path,
        cooldownKey: "audit",
      });
    }
    if (shouldThrowOnFailure) throw error;
  }
}

/** Writes one audit_log row + emits to auditBus. Best-effort by default. */
export function writeAuditLog(opts: AuditLogOptions): Promise<void> {
  return writeAuditLogs([opts]);
}
