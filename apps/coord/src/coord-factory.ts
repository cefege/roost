// T3.1 — multi-runtime coord factory. Returns a (Request, ctx?) =>
// Promise<Response> handler that any fetch-capable runtime (Bun.serve,
// Node http via fetch adapter, Cloudflare Workers, Deno, Vercel Edge)
// can consume. main.ts owns Bun-specific concerns (TLS, server.requestIP,
// signal handlers, file-system DB); this factory owns the protocol layer.

import {
  extractAuditMeta,
  preflightResponse,
  recordAuditTelemetry,
  securityOptionsForConfig,
  shouldPersistNonConnectAudit,
  SPA_AUDIT_TELEMETRY_PATH,
  wrapResponse,
  writeAuditLog,
  type NonConnectAuditSurface,
} from "./middleware/security.ts";
import { checkRateLimit } from "./middleware/rate-limit.ts";
import { buildConnectRouter } from "./connect/router.ts";
import { makeConnectBunHandler } from "./connect/bun-handler.ts";
import { startTerminalTitleHub } from "./terminal-title-hub.ts";
import { startLastActivityHub } from "./last-activity-hub.ts";
import { startAgentStatusHub, stopAgentStatusHub } from "./agent-status-hub.ts";
import { log } from "@roost/shared/log";
import type { KyselyDB } from "./db/connection.ts";
import type { Database } from "bun:sqlite";
import type { CoordKey } from "./coord-key.ts";
import type { CoordConfig } from "@roost/shared/config";
import type { JwtCache } from "./jwt.ts";
import type { CoordinatorMoveService } from "./coord-move/orchestrator.ts";
import type { CallerOrigin } from "./middleware/caller-origin.ts";
import type { EmailDeliveryService } from "./email-delivery.ts";
import type { PasswordWorkGate } from "./connect/password-work-gate.ts";
import type { PendingEventPublicationStore } from "./pending-event-publications.ts";

export interface CoordHandlerContext {
  origin: CallerOrigin;
  /** Optional override for the SPA-static dispatcher. Runtimes that
   * can't read from disk (Cloudflare Workers) supply their own. */
  spa?: (url: URL, method: string, acceptEncoding: string) => Promise<Response> | Response;
  /** Optional override for the DB export endpoint. */
  dbExport?: (origin: CallerOrigin) => Promise<Response> | Response;
  hsts?: boolean;
}

export interface CoordDeps {
  db: KyselyDB;
  sqlite: Database;
  coordKey: CoordKey;
  cfg: CoordConfig;
  jwtCache: JwtCache;
  passwordWorkGate: PasswordWorkGate;
  move?: CoordinatorMoveService;
  pendingPublications?: PendingEventPublicationStore;
  onKeyRevoked?: (fingerprint: string) => void;
  onWorkerDeletedFence?: (fingerprint: string) => void;
  onWorkerDeletedSyncScope?: (dashboardId: string, fingerprint: string) => void;
  onWorkerDeletedSocketClose?: (fingerprint: string) => void;
  email?: Pick<EmailDeliveryService, "encryptPayload">;
  onDashboardRevoked?: (dashboardId: string, fingerprint?: string) => void;
}

export interface CoordHandle {
  /** Top-level fetch handler. Wraps responses with security/CORS headers.
   * Audit log: Connect RPCs audited inside auth-interceptor (only place
   * with verified caller fp); non-Connect paths (db-export, SPA, 404)
   * audited here with callerFp:null. */
  fetch(req: Request, ctx?: CoordHandlerContext): Promise<Response>;
  /** Drop any in-process state. Per-runtime cleanup is the caller's job. */
  dispose(): void;
}

export function createCoord(deps: CoordDeps): CoordHandle {
  const connectRouter = buildConnectRouter(deps);
  const connectHandler = makeConnectBunHandler(connectRouter);

  // Coord-authoritative OSC terminal title: parse it off the relayed byte
  // stream and broadcast changes via Sync. Replaces the dead per-browser
  // onTitle path without requiring a headless grid.
  const stopTerminalTitleHub = startTerminalTitleHub();
  // Coord-authoritative last-activity timestamp: stamp it off the same relayed
  // byte stream (throttled), broadcast via Sync. Drives the sidebar "Last
  // activity" filter aging out idle open sessions.
  const stopLastActivityHub = startLastActivityHub();
  // Volatile coding-agent status: validate worker ownership, retain the latest
  // active revision per session, and clear it on terminal close.
  startAgentStatusHub({
    db: deps.db,
    pushAllowedOrigins: deps.cfg.pushAllowedOrigins,
    tenantRouteKey: deps.cfg.tenantRouteKey,
  });

  const secOpts = securityOptionsForConfig(deps.cfg, false);

  async function fetchHandler(req: Request, ctx?: CoordHandlerContext): Promise<Response> {
    const url = new URL(req.url);
    const origin = ctx?.origin ?? { clientIp: "unknown", onHost: false, listener: "direct" };
    const opts = { ...secOpts, hsts: ctx?.hsts ?? false };

    if (req.method === "OPTIONS") return preflightResponse(req, opts);

    const limited = checkRateLimit(req, origin.clientIp);
    if (limited) return wrapResponse(limited, req, opts);

    const auditMeta = extractAuditMeta(req);
    let resp: Response;
    let isConnect = false;
    let nonConnectSurface: NonConnectAuditSurface = "api";
    try {
      if (connectHandler.matches(url.pathname)) {
        isConnect = true;
        resp = await connectHandler.fetch(req, origin);
      } else if (req.method === "POST" && url.pathname.startsWith("/roost.")) {
        nonConnectSurface = "api";
        resp = new Response("not found", { status: 404 });
      } else if (url.pathname === "/api/db-export") {
        nonConnectSurface = "db-export";
        resp = ctx?.dbExport
          ? await ctx.dbExport(origin)
          : new Response(JSON.stringify({ error: "db-export not configured" }), { status: 501 });
      } else if (url.pathname.startsWith("/api/")) {
        nonConnectSurface = "api";
        resp = new Response("not found", { status: 404 });
      } else if (ctx?.spa) {
        nonConnectSurface = "spa";
        resp = await ctx.spa(url, req.method, req.headers.get("accept-encoding") ?? "");
      } else {
        resp = new Response("not found", { status: 404 });
      }
    } catch (e) {
      log.error("coord-factory", "fetch_error", { path: auditMeta.path, error: (e as Error).message });
      resp = new Response(JSON.stringify({ error: "internal" }), {
        status: 500, headers: { "content-type": "application/json" },
      });
    }

    // Connect RPCs audit inside the interceptor with the verified caller fp.
    // Static/deep-link successes retain one bounded metric label but do not
    // let arbitrary Internet paths amplify durable SQLite rows.
    if (!isConnect) {
      const telemetryPath = nonConnectSurface === "spa"
        ? SPA_AUDIT_TELEMETRY_PATH
        : auditMeta.path;
      recordAuditTelemetry(telemetryPath, resp.status);
      if (shouldPersistNonConnectAudit({
        surface: nonConnectSurface,
        method: auditMeta.method,
        status: resp.status,
      })) {
        writeAuditLog({
          db: deps.db,
          status: resp.status,
          ...auditMeta,
          callerFp: null,
          recordTelemetry: false,
        });
      }
    }
    return wrapResponse(resp, req, opts);
  }

  function dispose(): void {
    stopTerminalTitleHub();
    stopLastActivityHub();
    stopAgentStatusHub();
  }

  return { fetch: fetchHandler, dispose };
}
