// Security headers + CORS + audit log writes. Plain fetch-handler
// helpers — no H3 dependency.

import type { CoordConfig } from "@roost/shared/config";
// Exposed-to-JS header names are part of the SPA↔coord trust contract.
import { X_ROOST_AUTH_LAYER } from "@roost/shared/wire/headers";
import { TRACE_HEADER } from "@roost/shared/trace";
import { auditBus } from "../buses.ts";
import { recordRequest, recordError } from "../telemetry.ts";
import { signal } from "@roost/shared/diag";
import type { KyselyDB } from "../db/connection.ts";
import type { ListenerTrust } from "./caller-origin.ts";

const TURNSTILE_ORIGIN = "https://challenges.cloudflare.com";
const CSP_TAIL = "frame-ancestors 'none'";

export interface SecurityOptions {
  relaxedCsp: boolean;
  corsAllowedOrigins: string[];
  hsts: boolean;
  connectOrigins: string[];
  managed: boolean;
}

export function securityOptionsForConfig(cfg: CoordConfig, hsts: boolean): SecurityOptions {
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
    corsAllowedOrigins: cfg.corsAllowedOrigins,
    hsts,
    connectOrigins: [...origins],
    managed: cfg.saasMode,
  };
}

export function buildCsp(
  relaxed: boolean,
  connectOrigins: string[],
  managed = false,
): string {
  const connections = new Set(["'self'", ...connectOrigins]);
  if (relaxed) {
    connections.add("http:");
    connections.add("ws:");
  }
  const scriptSources = ["'self'", "'wasm-unsafe-eval'", "blob:"];
  const directives = [
    "default-src 'self'",
    `script-src ${managed ? [...scriptSources, TURNSTILE_ORIGIN].join(" ") : scriptSources.join(" ")}`,
    "worker-src 'self' blob:",
    "style-src 'self' 'unsafe-inline'",
    "img-src 'self' data:",
    "font-src 'self' data:",
    "base-uri 'self'",
    "form-action 'none'",
    "object-src 'none'",
  ];
  if (managed) directives.push(`frame-src ${TURNSTILE_ORIGIN}`);
  return `${directives.join("; ")}; connect-src ${[...connections].join(" ")}; ${CSP_TAIL}`;
}

export function applySecurityHeaders(
  headers: Headers,
  relaxed: boolean,
  hsts: boolean,
  connectOrigins: string[],
  managed = false,
): void {
  headers.set("content-security-policy", buildCsp(relaxed, connectOrigins, managed));
  headers.set("x-frame-options", "DENY");
  headers.set("x-content-type-options", "nosniff");
  headers.set("referrer-policy", "no-referrer");
  headers.set("permissions-policy", "camera=(), geolocation=(), microphone=(self)");
  if (hsts) headers.set("strict-transport-security", "max-age=31536000");
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
  applySecurityHeaders(headers, opts.relaxedCsp, opts.hsts, opts.connectOrigins, opts.managed);
  return new Response(resp.body, { status: resp.status, statusText: resp.statusText, headers });
}

export function preflightResponse(req: Request, opts: SecurityOptions): Response {
  const headers = new Headers();
  applyCors(headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(headers, opts.relaxedCsp, opts.hsts, opts.connectOrigins, opts.managed);
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

/** Public anonymous credential failures are represented by bounded telemetry
 * and cooldown-coalesced signals rather than attacker-amplified SQLite rows. */
export function shouldPersistConnectAudit(opts: {
  listener: ListenerTrust;
  status: number;
  callerFp: string | null;
}): boolean {
  return !(
    opts.listener === "public-edge"
    && opts.status === 401
    && opts.callerFp === null
  );
}

export function recordAuditTelemetry(path: string, status: number): void {
  recordRequest(path);
  if (status >= 400) recordError(path);
}


/**
 * Writes one audit_log row + emits to auditBus. Best-effort: any
 * write failure is swallowed.
 */
export function writeAuditLog(opts: {
  db: KyselyDB;
  status: number;
  method: string;
  path: string;
  traceId: string | undefined;
  callerFp: string | null;
  /** Server-confirmed tenant scope when the caller has a selected actor. */
  dashboardId?: string | null;
  /** Terminal input uses strict mode so a completed write cannot be reported
   * without an explicit audit-persistence outcome. Other request audits remain
   * best-effort to avoid changing interceptor failure semantics. */
  throwOnFailure?: boolean;
  /** Set false when the caller already recorded telemetry before applying a
   * durable-audit predicate. */
  recordTelemetry?: boolean;
}): Promise<void> {
  if (opts.recordTelemetry !== false) recordAuditTelemetry(opts.path, opts.status);
  return opts.db
    .insertInto("audit_log")
    .values({
      ts: Date.now(),
      caller_fp: opts.callerFp,
      dashboard_id: opts.dashboardId ?? null,
      method: opts.method,
      path: opts.path,
      status: opts.status,
      trace_id: opts.traceId ?? null,
    })
    .returning(["id", "dashboard_id", "ts", "caller_fp", "method", "path", "status", "trace_id"])
    .executeTakeFirst()
    .then((inserted) => {
      if (!inserted) return;
      auditBus.publish({
        id: inserted.id as number,
        _dashboard_id: (inserted.dashboard_id as string | null) ?? undefined,
        ts: inserted.ts as number,
        caller_fp: (inserted.caller_fp as string | null) ?? null,
        caller_label: null,
        method: inserted.method as string,
        path: inserted.path as string,
        status: inserted.status as number,
        trace_id: (inserted.trace_id as string | null) ?? null,
      });
    })
    .catch((e) => {
      signal("audit.write_failed", { error: String(e), path: opts.path, cooldownKey: "audit" });
      if (opts.throwOnFailure) throw e;
    });
}
