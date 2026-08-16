// Security headers + CORS + audit log writes. Plain fetch-handler
// helpers — no H3 dependency.

import type { KyselyDB } from "../db/connection.ts";
import { TRACE_HEADER } from "@roost/shared/trace";
import { auditBus } from "../buses.ts";
import { recordRequest, recordError } from "../telemetry.ts";
import { signal } from "@roost/shared/diag";
import type { CoordConfig } from "@roost/shared/config";

const CSP_BASE = [
  "default-src 'self'",
  "script-src 'self' 'wasm-unsafe-eval' blob:",
  "worker-src 'self' blob:",
  "style-src 'self' 'unsafe-inline'",
  "img-src 'self' data:",
  "font-src 'self' data:",
  "base-uri 'self'",
  "form-action 'none'",
  "object-src 'none'",
].join("; ");
const CSP_TAIL = "frame-ancestors 'none'";

export interface SecurityOptions {
  relaxedCsp: boolean;
  corsAllowedOrigins: string[];
  hsts: boolean;
  connectOrigins: string[];
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
  };
}

export function buildCsp(relaxed: boolean, connectOrigins: string[]): string {
  const connections = new Set(["'self'", ...connectOrigins]);
  if (relaxed) {
    connections.add("http:");
    connections.add("ws:");
  }
  return `${CSP_BASE}; connect-src ${[...connections].join(" ")}; ${CSP_TAIL}`;
}

export function applySecurityHeaders(
  headers: Headers,
  relaxed: boolean,
  hsts: boolean,
  connectOrigins: string[],
): void {
  headers.set("content-security-policy", buildCsp(relaxed, connectOrigins));
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
    headers.set("access-control-expose-headers", "x-roost-auth-layer");
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
  /** Terminal input uses strict mode so a completed write cannot be reported
   * without an explicit audit-persistence outcome. Other request audits remain
   * best-effort to avoid changing interceptor failure semantics. */
  throwOnFailure?: boolean;
}): Promise<void> {
  recordRequest(opts.path);
  if (opts.status >= 400) recordError(opts.path);
  return opts.db
    .insertInto("audit_log")
    .values({
      ts: Date.now(),
      caller_fp: opts.callerFp,
      method: opts.method,
      path: opts.path,
      status: opts.status,
      trace_id: opts.traceId ?? null,
    })
    .returning(["id", "ts", "caller_fp", "method", "path", "status", "trace_id"])
    .executeTakeFirst()
    .then((inserted) => {
      if (!inserted) return;
      auditBus.publish({
        id: inserted.id as number,
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
