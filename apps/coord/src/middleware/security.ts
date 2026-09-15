// Security headers + CORS + audit log writes. Plain fetch-handler
// helpers — no H3 dependency.

import { DEFAULT_WORKER_LOCAL_UI_ORIGIN, type CoordConfig } from "@roost/shared/config";
// Exposed-to-JS header names are part of the SPA↔coord trust contract.
import { X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import { TRACE_HEADER } from "@roost/shared/trace";
import { auditBus } from "../buses.ts";
import { recordRequest, recordError } from "../telemetry.ts";
import { signal } from "@roost/shared/diag";
import type { KyselyDB } from "../db/connection.ts";
import type { ListenerTrust } from "./caller-origin.ts";
import { applySecurityHeaders } from "@roost/shared/http-security";

export interface SecurityOptions {
  relaxedCsp: boolean;
  corsAllowedOrigins: string[];
  hsts: boolean;
  connectOrigins: string[];
}

/** The worker-served loopback SPA is a first-class browser origin: its fetches
 * carry a bearer JWT and no cookies, so allowing it grants reachability only. */
export function securityOptionsForConfig(cfg: CoordConfig, hsts: boolean): SecurityOptions {
  const corsAllowedOrigins = [DEFAULT_WORKER_LOCAL_UI_ORIGIN, ...cfg.corsAllowedOrigins];
  const origins = new Set<string>([
    "https://api.deepgram.com",
    "wss://api.deepgram.com",
  ]);
  for (const raw of [cfg.publicUrl, cfg.webPublicUrl, ...cfg.corsAllowedOrigins]) {
    if (!raw) continue;
    const origin = new URL(raw).origin;
    origins.add(origin);
    if (origin.startsWith("https://")) origins.add(`wss://${origin.slice("https://".length)}`);
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
  const headers = new Headers(resp.headers);
  applyCors(headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(headers, opts.relaxedCsp, opts.hsts, opts.connectOrigins);
  return new Response(resp.body, { status: resp.status, statusText: resp.statusText, headers });
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

export type NonConnectAuditSurface = "spa" | "db-export" | "api";

/** Successful static/deep-link reads have no durable forensic value. Errors
 * and explicit API/export surfaces remain auditable. */
export function shouldPersistNonConnectAudit(opts: {
  surface: NonConnectAuditSurface;
  method: string;
  status: number;
}): boolean {
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
