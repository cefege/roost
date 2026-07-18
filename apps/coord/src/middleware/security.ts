// Security headers + CORS + audit log writes. Plain fetch-handler
// helpers — no H3 dependency.

import type { KyselyDB } from "../db/connection.ts";
import { TRACE_HEADER } from "@roost/shared/trace";
import { auditBus } from "../buses.ts";
import { recordRequest, recordError } from "../telemetry.ts";
import { signal } from "@roost/shared/diag";

// blob: in script-src + worker-src lets the voice-capture AudioWorklet load its
// processor module from a blob URL (Safari enforces this; Chrome is lenient).
const CSP_BASE = "default-src 'self'; script-src 'self' 'unsafe-inline' 'wasm-unsafe-eval' blob:; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data:; font-src 'self' data:";
const CSP_TAIL = "frame-ancestors 'none'";
const CONNECT_PROD = "connect-src 'self' wss: https:";
const CONNECT_RELAXED = "connect-src 'self' wss: https: ws: http:";

function buildCsp(relaxed: boolean): string {
  return `${CSP_BASE}; ${relaxed ? CONNECT_RELAXED : CONNECT_PROD}; ${CSP_TAIL}`;
}

/** Apply CSP + frame-options + nosniff to a Headers object. */
function applySecurityHeaders(headers: Headers, relaxed: boolean): void {
  headers.set("content-security-policy", buildCsp(relaxed));
  headers.set("x-frame-options", "DENY");
  headers.set("x-content-type-options", "nosniff");
}

/** Apply CORS headers given the request's origin and the allowlist. */
function applyCors(headers: Headers, reqOrigin: string | null, allowedOrigins: string[]): void {
  if (allowedOrigins.length === 0) {
    headers.set("access-control-allow-origin", "*");
  } else if (reqOrigin && allowedOrigins.includes(reqOrigin)) {
    headers.set("access-control-allow-origin", reqOrigin);
  }
  headers.set("vary", "origin, access-control-request-method, access-control-request-headers");
  headers.set("access-control-allow-methods", "*");
  headers.set("access-control-allow-headers", "*");
}

/** Wrap a Response with our standard security + CORS headers in-place. */
export function wrapResponse(resp: Response, req: Request, opts: { relaxedCsp: boolean; corsAllowedOrigins: string[] }): Response {
  const headers = new Headers(resp.headers);
  applyCors(headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(headers, opts.relaxedCsp);
  return new Response(resp.body, { status: resp.status, statusText: resp.statusText, headers });
}

/** OPTIONS preflight 204 response with CORS + security headers. */
export function preflightResponse(req: Request, opts: { relaxedCsp: boolean; corsAllowedOrigins: string[] }): Response {
  const headers = new Headers();
  applyCors(headers, req.headers.get("origin"), opts.corsAllowedOrigins);
  applySecurityHeaders(headers, opts.relaxedCsp);
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
}): void {
  recordRequest(opts.path);
  if (opts.status >= 400) recordError(opts.path);
  void opts.db
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
    .catch((e) => signal("audit.write_failed", { error: String(e), path: opts.path, cooldownKey: "audit" }));
}
