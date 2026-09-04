// Owns Bun HTTP and WebSocket listener construction for coordinator startup.
// runCoord calls it after the database, protocol services, and transports are ready.
// It depends on Bun.serve plus the coordinator transport and public-surface adapters.
// The primary listener must be live before runCoord starts move recovery.

import type { CoordConfig } from "@roost/shared/config";
import { log } from "@roost/shared/log";
import type { Server, ServerWebSocket } from "bun";
import type { Database } from "bun:sqlite";
import { randomUUID } from "node:crypto";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { dirname, join } from "node:path";
import {
  COORD_WEBSOCKET_MAX_PAYLOAD_BYTES,
  handleWorkerWsUpgrade,
  type WorkerWsData,
} from "./connect/worker-ws-handler.ts";
import {
  handleSyncWsUpgrade,
  type SyncWsData,
} from "./connect/sync-ws-handler.ts";
import type { WorkerServiceDeps } from "./connect/worker-service.ts";
import type { ConnectDeps } from "./connect/router.ts";
import type { CoordHandle } from "./coord-factory.ts";
import { handleInternalHandoffRequest } from "./coord-move/internal-http.ts";
import type { CoordinatorMoveService } from "./coord-move/orchestrator.ts";
import { createSqliteSnapshot } from "./db/snapshot.ts";
import { resolveCallerOrigin, type CallerOrigin, type ListenerTrust } from "./middleware/caller-origin.ts";
import { makeCfAccessVerifier } from "./middleware/cf-access.ts";
import { coordinatorAvailabilityResponse } from "./middleware/coordinator-availability.ts";
import { makePublicSurface } from "./middleware/public-surface.ts";

interface WorkerWebSocketDispatch {
  open(ws: ServerWebSocket<WorkerWsData>): void;
  message(ws: ServerWebSocket<WorkerWsData>, message: string | Buffer): void;
  close(ws: ServerWebSocket<WorkerWsData>): void;
}

interface SyncWebSocketDispatch {
  open(ws: ServerWebSocket<SyncWsData>): void;
  message(ws: ServerWebSocket<SyncWsData>, message: string | Buffer): void;
  drain(ws: ServerWebSocket<SyncWsData>): void;
  close(ws: ServerWebSocket<SyncWsData>): void;
}

interface BunCoordinatorListenerDeps {
  cfg: CoordConfig;
  coord: CoordHandle;
  sqlite: Database;
  move: CoordinatorMoveService;
  workerDeps: WorkerServiceDeps;
  syncDeps: ConnectDeps;
  workerWs: WorkerWebSocketDispatch;
  syncWs: SyncWebSocketDispatch;
  spa: (url: URL, method: string, acceptEncoding: string) => Promise<Response> | Response;
}

interface BunCoordinatorListeners {
  server: Server<WorkerWsData | SyncWsData>;
  publicServer: Server<WorkerWsData | SyncWsData> | undefined;
  host: string;
  tlsEnabled: boolean;
}

export function startBunCoordinatorListeners(
  deps: BunCoordinatorListenerDeps,
): BunCoordinatorListeners {
  const {
    cfg,
    coord,
    sqlite,
    move,
    workerDeps,
    syncDeps,
    workerWs,
    syncWs,
    spa,
  } = deps;

  // ONE Bun websocket handler multiplexing both raw-WS transports. Dispatch on
  // the discriminant stamped at upgrade (ws.data.kind). ServerWebSocket is
  // invariant on `data`, so the compiler can't narrow the socket handle from
  // ws.data.kind alone — cast to the known variant after the discriminant check.
  const websocket = {
    // One shared ceiling covers both worker and browser Sync sockets on both
    // listeners. Bun rejects the offending frame/socket before dispatch.
    maxPayloadLength: COORD_WEBSOCKET_MAX_PAYLOAD_BYTES,
    open(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.open(ws as ServerWebSocket<SyncWsData>);
      else workerWs.open(ws as ServerWebSocket<WorkerWsData>);
    },
    message(ws: ServerWebSocket<WorkerWsData | SyncWsData>, msg: string | Buffer): void {
      if (ws.data.kind === "sync") syncWs.message(ws as ServerWebSocket<SyncWsData>, msg);
      else workerWs.message(ws as ServerWebSocket<WorkerWsData>, msg);
    },
    drain(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.drain(ws as ServerWebSocket<SyncWsData>);
    },
    close(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.close(ws as ServerWebSocket<SyncWsData>);
      else workerWs.close(ws as ServerWebSocket<WorkerWsData>);
    },
  };
  const publicAccess = cfg.cfAccessTeamDomain && cfg.cfAccessAud
    ? makeCfAccessVerifier(cfg.cfAccessTeamDomain, cfg.cfAccessAud)
    : undefined;
  const publicSurface = cfg.publicBind
    ? makePublicSurface({
        access: publicAccess,
        coord,
        syncDeps,
        workerDeps,
        move,
        cfg,
        spa,
        syncUpgrade: handleSyncWsUpgrade,
        workerUpgrade: (req, server, workerSurfaceDeps) => handleWorkerWsUpgrade(
          req,
          server as unknown as Server<WorkerWsData>,
          workerSurfaceDeps,
        ),
      })
    : null;

  async function dbExportResponse(origin: CallerOrigin): Promise<Response> {
    if (cfg.saasMode) {
      return new Response("not found", { status: 404 });
    }
    if (!origin.onHost) {
      return new Response(JSON.stringify({ error: "on-host only" }), {
        status: 403, headers: { "content-type": "application/json" },
      });
    }
    if (!existsSync(cfg.dbPath)) return new Response(null, { status: 404 });

    const snapshotPath = join(dirname(cfg.dbPath), `.coord-export-${randomUUID()}.db`);
    try {
      const { size } = createSqliteSnapshot(sqlite, snapshotPath);
      const body = await Bun.file(snapshotPath).arrayBuffer();
      return new Response(body, {
        status: 200,
        headers: {
          "content-type": "application/x-sqlite3",
          "content-length": String(size),
          "content-disposition": `attachment; filename="coordinator_v2.db"`,
        },
      });
    } finally {
      rmSync(snapshotPath, { force: true });
    }
  }

  const [host, portStr] = cfg.bind.split(":") as [string, string];
  const port = parseInt(portStr, 10);

  // tailscale serve overwrites X-Forwarded-For with the authenticated tailnet
  // address. Trust that header only on this boot-configured listener profile.
  const trustProxy = cfg.trustProxy;
  const listenerTrust: ListenerTrust = trustProxy ? "tailscale-serve" : "direct";

  const tls = cfg.tlsCertPath && cfg.tlsKeyPath
    ? {
        cert: readFileSync(cfg.tlsCertPath),
        key: readFileSync(cfg.tlsKeyPath),
        // ponytail: ALPN dropped 2026-06-22. Bun.serve alpnProtocols
        // is a no-op in 1.3.14 (openssl shows "No ALPN negotiated"
        // regardless). Worker transport is now raw WS, not Connect-bidi
        // h2, so ALPN is dead code. Removing fixes ERR_SSL_PROTOCOL_ERROR
        // on some Chrome profiles.
      }
    : undefined;

  let econnresetCount = 0;
  let econnresetTimer: ReturnType<typeof setTimeout> | null = null;
  function handleEconnreset(): void {
    econnresetCount++;
    if (!econnresetTimer) {
      econnresetTimer = setTimeout(() => {
        log.debug("server", "tls_handshake_probes", { count: econnresetCount });
        econnresetCount = 0;
        econnresetTimer = null;
      }, 60_000);
    }
  }

  const server = Bun.serve({
    hostname: host, port, tls,
    // idleTimeout reaps connections with no traffic for N seconds. This is
    // the fix for "internet blipped → browser can't reconnect, even reload
    // hangs": after a network drop Chrome keeps reusing a ZOMBIE HTTP/2
    // connection (idleTimeout:0 meant coord never dropped it), so every
    // request — including a page reload — queues on a dead socket forever.
    // A finite timeout makes coord RST the dead connection → Chrome opens a
    // fresh one → recovery, no coord bounce needed.
    //
    // Why a finite cap is now safe: every long-lived WS sees traffic well
    // under 120s. The worker bidi is pinged every 30s (worker-conn.ts) and
    // the browser Sync WS now receives a keepalive every 30s
    // (sync-ws-handler.ts). Bun's idleTimeout resets on any received WS
    // message, so the keepalive data frame keeps healthy connections alive;
    // only genuinely-dead/half-open connections (3+ missed keepalives, never
    // reaped by the browser because the close frame can't traverse a dead
    // TCP) get reaped server-side → close() → feed.dispose(), also closing
    // the pre-existing slow SyncFeed subscription leak on coord.
    // Healthy browsers also poll coord health every 5s (sync.ts
    // HEALTH_POLL_INTERVAL_MS), so they're never idle either — only
    // genuinely dead connections hit the cap.
    idleTimeout: 120,
    // The worker link is a long-lived request carrying events and PTY bytes.
    // Its body grows without bound, so use a request cap above any realistic
    // connection volume to avoid terminating the stream mid-session.
    maxRequestBodySize: 1024 * 1024 * 1024 * 256, // 256 GiB
    async fetch(req, listenerServer) {
      const internal = await handleInternalHandoffRequest(req, move);
      // Above the retired gate: this route carries its own constant-time
      // secret auth and executes internalCommit/internalAbort side effects,
      // so 410-ing its response would drop a commit that already happened.
      if (internal) return internal;
      const path = new URL(req.url).pathname;
      const unavailable = coordinatorAvailabilityResponse(move.gate.mode, req.method, path);
      if (unavailable) return unavailable;
      // Worker raw-WS transport (/ws/coord-worker/:fp). If this is that
      // upgrade, authenticate + hijack here (Bun-specific); null = not our
      // path → fall through to the portable coord.fetch.
      const ws = await handleWorkerWsUpgrade(req, listenerServer, workerDeps);
      if (ws !== null) return ws;
      // Browser Sync firehose raw-WS (/ws/coord-sync). Same hijack pattern;
      // null = not our path → fall through to the portable coord.fetch.
      const sws = await handleSyncWsUpgrade(req, listenerServer, syncDeps);
      if (sws !== null) return sws;
      // The Sync firehose moved to /ws/coord-sync (raw WS). Reject the legacy
      // Connect server-streaming Sync RPC HERE — at the fetch layer, BEFORE
      // Connect establishes the streaming response. A throwing stub inside the
      // handler still lets Connect OPEN the stream, leaving the Bun 1.3.14
      // RequestContext.onAbort use-after-free reachable when an old-bundle
      // straggler aborts it. A plain unary Response is never abort-tracked, so
      // the crash path is gone. Old SPA bundles get an error → fail-fast →
      // reload onto the WS client.
      if (new URL(req.url).pathname === "/roost.v1.CoordinatorService/Sync") {
        return new Response("sync moved to /ws/coord-sync", { status: 410 });
      }
      const origin = resolveCallerOrigin(
        listenerTrust,
        listenerServer.requestIP(req)?.address,
        req.headers,
      );
      return coord.fetch(req, {
        origin,
        spa,
        dbExport: dbExportResponse,
        hsts: Boolean(tls) || trustProxy,
      });
    },
    websocket,
    error(err: Error & { code?: string; errno?: number; syscall?: string; address?: string; port?: number }) {
      if (err.code === "ECONNRESET") {
        handleEconnreset();
        return new Response(null, { status: 0 });
      }
      log.error("server", "server_error", {
        code: err.code, errno: err.errno, syscall: err.syscall,
        address: err.address, port: err.port, message: err.message,
      });
      return new Response(JSON.stringify({ error: "internal server error" }), {
        status: 500, headers: { "content-type": "application/json" },
      });
    },
  });

  let publicServer: Server<WorkerWsData | SyncWsData> | undefined;
  if (cfg.publicBind && publicSurface) {
    const [publicHost, publicPortStr] = cfg.publicBind.split(":") as [string, string];
    publicServer = Bun.serve({
      hostname: publicHost,
      port: Number(publicPortStr),
      idleTimeout: 120,
      maxRequestBodySize: 16 * 1024 * 1024,
      websocket,
      fetch: publicSurface.fetch,
      error: publicSurface.error,
    });
    log.info("main", "public_listening", {
      bind: `${publicHost}:${publicServer.port}`,
      policy: publicAccess ? "cloudflare-access" : "managed",
      access_team: cfg.cfAccessTeamDomain ?? null,
    });
  }

  return {
    server,
    publicServer,
    host,
    tlsEnabled: !!tls,
  };
}
