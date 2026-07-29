// Coord entry point — Bun-specific wrapper. The protocol/fetch layer
// lives in coord-factory.ts (T3.1) so other runtimes can reuse it.

import { loadCoordConfig, type CoordConfig } from "@roost/shared/config";
import { openDb } from "./db/connection.ts";
import { runMigrations } from "./db/migrate.ts";
import { loadOrCreateCoordKey } from "./coord-key.ts";
import { importAuthorizedKeys } from "./authorized-keys.ts";
import { newJwtCache } from "./jwt.ts";
import { scheduleBackups } from "./backup.ts";
import { scheduleAuditRetention } from "./audit-retention.ts";
import {
  AGENT_TRANSCRIPT_RETENTION_DAYS,
  scheduleAgentTranscriptRetention,
} from "./agent-transcript.ts";
import { installByteHubBusHook } from "./byte-hub.ts";
import { createCoord } from "./coord-factory.ts";
import { handleWorkerWsUpgrade, makeWorkerWsHandler, type WorkerWsData } from "./connect/worker-ws-handler.ts";
import { handleSyncWsUpgrade, makeSyncWsHandler, type SyncWsData } from "./connect/sync-ws-handler.ts";
import { CoordinatorMoveOrchestrator } from "./coord-move/orchestrator.ts";
import { HandoffStateStore } from "./coord-move/state.ts";
import { createBunCoordinatorMoveRuntime } from "./coord-move/bun-runtime.ts";
import { handleInternalHandoffRequest } from "./coord-move/internal-http.ts";
import { connectWorkers } from "./connect/worker-registry.ts";
import type { WorkerServiceDeps } from "./connect/worker-service.ts";
import type { ServerWebSocket } from "bun";
import { workspaceBus } from "./buses.ts";
import { asWorkspaceId } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { existsSync, readFileSync, statSync } from "node:fs";
import { WEB_ASSETS } from "./web-embed.generated.ts";
import { createSpaResponder } from "./spa.ts";
import { MIGRATIONS } from "./migrations-embed.generated.ts";


export async function runCoord() {
  const bootMs = Date.now();

  let cfg: CoordConfig;
  try {
    cfg = loadCoordConfig(process.env as Record<string, string | undefined>);
  } catch (e) {
    console.error(JSON.stringify({ ev: "config_error", error: (e as Error).message }));
    process.exit(1);
  }

  const { db, sqlite } = openDb(cfg.dbPath);
  await runMigrations(sqlite, MIGRATIONS.length > 0 ? MIGRATIONS : undefined);
  log.info("main", "db_ready", { path: cfg.dbPath });

  try {
    // Closed sessions are DELETED, not parked (no "closed" limbo). Purge any
    // already-closed rows (legacy 'closed' data + safety net). NEVER touch
    // 'open' rows here — a live long-running terminal must never be deleted by
    // a janitor; truly-dead open sessions are reconciled by the worker
    // snapshot's ghost-close on reconnect, not by a wall-clock age cutoff.
    const closed = await db
      .deleteFrom("sessions")
      .where("status", "=", "closed")
      .executeTakeFirst();
    await db
      .deleteFrom("workspace_sessions")
      .where("workspace_id", "not in", db
        .selectFrom("workspace_sessions as ws")
        .innerJoin("sessions as s", "s.id", "ws.session_id")
        .where("s.status", "=", "open").select("ws.workspace_id"))
      .execute();
    // Capture orphan ids BEFORE the delete so workspaceBus subscribers
    // (SPA sync stream) get the `deleted` deltas — without this, SPAs
    // that survive a coord restart see stale workspace rows in the
    // sidebar until the user reloads the tab.
    const orphanRows = await db
      .selectFrom("workspaces")
      .select("id")
      .where("id", "not in", db.selectFrom("workspace_sessions").select("workspace_id").distinct())
      .execute();
    const orphans = await db
      .deleteFrom("workspaces")
      .where("id", "not in", db.selectFrom("workspace_sessions").select("workspace_id").distinct())
      .executeTakeFirst();
    for (const r of orphanRows) {
      workspaceBus.publish({ kind: "deleted", id: asWorkspaceId(r.id as string) });
    }
    log.info("main", "janitor", {
      deleted_sessions: Number(closed?.numDeletedRows ?? 0),
      pruned_orphan_workspaces: Number(orphans?.numDeletedRows ?? 0),
    });
  } catch (e) {
    log.warn("main", "janitor_failed", { error: (e as Error).message });
  }

  const coordKey = await loadOrCreateCoordKey(cfg.coordKeyPath);
  log.info("main", "coord_key_ready", { kid: coordKey.verifyingKeyKid() });

  if (cfg.authorizedKeysPath && existsSync(cfg.authorizedKeysPath)) {
    try {
      const n = await importAuthorizedKeys(db, cfg.authorizedKeysPath);
      log.info("main", "authorized_keys_imported", { count: n, path: cfg.authorizedKeysPath });
    } catch (e) {
      log.warn("main", "authorized_keys_import_failed", { error: (e as Error).message });
    }
  }

  const jwtCache = newJwtCache();
  let publishRelocation: ((handoffId: string, sourceUrl: string, targetUrl: string) => void) | null = null;
  const move = new CoordinatorMoveOrchestrator({
    cfg,
    coordKey,
    store: new HandoffStateStore(cfg.handoffPath),
    runtime: createBunCoordinatorMoveRuntime({
      sqlite,
      dbPath: cfg.dbPath,
      coordKeyPath: cfg.coordKeyPath,
      authorizedKeysPath: cfg.authorizedKeysPath,
      handoffPath: cfg.handoffPath,
      publishRelocation: (state) => publishRelocation?.(state.handoffId, state.sourceUrl, state.targetUrl),
    }),
    workers: async () => (await db.selectFrom("workers").select(["fp", "label", "os", "git_sha", "reachable_addr"]).execute())
      .map((worker) => ({
        fp: worker.fp,
        label: worker.label,
        os: worker.os,
        gitSha: worker.git_sha,
        reachableAddr: worker.reachable_addr,
        online: connectWorkers.has(worker.fp),
      })),
  });
  const coord = createCoord({ db, sqlite, coordKey, cfg, jwtCache, move });
  const spaResponse = createSpaResponder(cfg.webDistPath, WEB_ASSETS);

  // Raw-WS worker transport deps (Bun-specific; coord-factory stays
  // fetch-only/portable). The WS handler (worker-ws-handler.ts) reuses the
  // shared worker-conn registry + makeWorkerConn from worker-service.ts.
  const wsDeps: WorkerServiceDeps = { db, sqlite, coordKey, jwtCache, cfg, move };
  const workerWs = makeWorkerWsHandler(wsDeps);
  // Sync firehose raw-WS (/ws/coord-sync). Same wsDeps shape (ConnectDeps ⊇
  // { db, coordKey, cfg, jwtCache }); the feed itself is shared with the
  // former Connect sync via startSyncFeed (handlers-streaming.ts).
  const syncWs = makeSyncWsHandler({ ...wsDeps, move });
  publishRelocation = (handoffId, sourceUrl, targetUrl) => syncWs.publishRelocation(handoffId, sourceUrl, targetUrl);

  // ONE Bun websocket handler multiplexing both raw-WS transports. Dispatch on
  // the discriminant stamped at upgrade (ws.data.kind). ServerWebSocket is
  // invariant on `data`, so the compiler can't narrow the socket handle from
  // ws.data.kind alone — cast to the known variant after the discriminant check.
  const websocket = {
    open(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.open(ws as ServerWebSocket<SyncWsData>);
      else workerWs.open(ws as ServerWebSocket<WorkerWsData>);
    },
    message(ws: ServerWebSocket<WorkerWsData | SyncWsData>, msg: string | Buffer): void {
      if (ws.data.kind === "sync") syncWs.message(ws as ServerWebSocket<SyncWsData>, msg);
      else workerWs.message(ws as ServerWebSocket<WorkerWsData>, msg);
    },
    close(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.close(ws as ServerWebSocket<SyncWsData>);
      else workerWs.close(ws as ServerWebSocket<WorkerWsData>);
    },
  };


  function dbExportResponse(clientIp: string | undefined): Response {
    if (!clientIp || !["127.0.0.1", "::1", "::ffff:127.0.0.1"].includes(clientIp)) {
      return new Response(JSON.stringify({ error: "loopback only" }), {
        status: 403, headers: { "content-type": "application/json" },
      });
    }
    if (!existsSync(cfg.dbPath)) return new Response(null, { status: 404 });
    const stat = statSync(cfg.dbPath);
    return new Response(Bun.file(cfg.dbPath), {
      status: 200,
      headers: {
        "content-type": "application/x-sqlite3",
        "content-length": String(stat.size),
        "content-disposition": `attachment; filename="coordinator_v2.db"`,
      },
    });
  }

  const [host, portStr] = cfg.bind.split(":") as [string, string];
  const port = parseInt(portStr, 10);

  // ROOST_TRUST_PROXY=1: coord runs PLAINTEXT on loopback behind `tailscale
  // serve` (TLS terminated there). This dodges the Bun 1.3.14 native segfault
  // in us_internal_ssl_on_close / RequestContext.onAbort that fires when a
  // browser aborts a long-lived streaming TLS response (the Sync firehose) —
  // Bun never runs the TLS close path. tailscale serve OVERWRITES
  // X-Forwarded-For with the authenticated tailnet IP (verified: a
  // client-supplied XFF is replaced, not appended), so xff[0] is the
  // un-spoofable real client IP. Direct on-host calls carry no XFF → we keep
  // the loopback socket peer so assertLoopback (db-export) stays strict.
  const trustProxy = process.env.ROOST_TRUST_PROXY === "1";

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

  // Install BEFORE Bun.serve accepts requests: a worker reconnecting
  // via the worker WS immediately at boot would otherwise emit
  // its first `opened` events with no byte-hub subscriber listening,
  // dropping bytes for those sessions as drop_unmapped_chunk until
  // the next snapshot.
  installByteHubBusHook();

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
    async fetch(req, server) {
      const internal = await handleInternalHandoffRequest(req, move);
      // Above the retired gate: this route carries its own constant-time
      // secret auth and executes internalCommit/internalAbort side effects,
      // so 410-ing its response would drop a commit that already happened.
      if (internal) return internal;
      const path = new URL(req.url).pathname;
      const retiredDiscoveryPath = path === "/roost.v1.CoordinatorService/AuthCoordIdentity"
        || path === "/roost.v1.CoordinatorService/AuthMintCoordinatorRelocation"
        || path === "/roost.v1.CoordinatorService/CoordinatorMoveStatus"
        // Public, leaks nothing, and 410-ing it paints the SPA's red
        // "Coordinator unreachable" banner over the relocation in progress.
        || path === "/roost.v1.CoordinatorService/MiscHealth";
      if (move.gate.mode === "retired" && req.method !== "GET" && !retiredDiscoveryPath) {
        return new Response("coordinator relocated", { status: 410 });
      }
      // Worker raw-WS transport (/ws/coord-worker/:fp). If this is that
      // upgrade, authenticate + hijack here (Bun-specific); null = not our
      // path → fall through to the portable coord.fetch.
      const ws = await handleWorkerWsUpgrade(req, server, wsDeps);
      if (ws !== null) return ws;
      // Browser Sync firehose raw-WS (/ws/coord-sync). Same hijack pattern;
      // null = not our path → fall through to the portable coord.fetch.
      const sws = await handleSyncWsUpgrade(req, server, wsDeps);
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
      let clientIp = server.requestIP(req)?.address;
      if (trustProxy) {
        const xff = req.headers.get("x-forwarded-for");
        if (xff) clientIp = xff.split(",")[0]!.trim();
      }
      return coord.fetch(req, {
        clientIp,
        spa: spaResponse,
        dbExport: dbExportResponse,
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

  log.info("main", "listening", { bind: `${host}:${server.port}`, tls: !!tls, http2: !!tls, uptime_ms: Date.now() - bootMs });
  // AFTER Bun.serve: recovery stages/commits/aborts workers, and `online` is
  // computed from the worker-WS registry this server populates. Running it
  // first guarantees an empty registry, an immediate `worker offline`, and a
  // blind rollback — plus up to ~15s of delayed first byte.
  void move.recover().catch((error) => log.error("coord", "move_recover_failed", { error: String(error) }));

  scheduleBackups(cfg.dbPath);
  scheduleAuditRetention(sqlite, cfg.auditRetentionDays);
  scheduleAgentTranscriptRetention(sqlite, AGENT_TRANSCRIPT_RETENTION_DAYS);

  const shutdown = (): void => {
    log.info("main", "shutdown");
    server.stop(true);
    coord.dispose();
    try { sqlite.close(); } catch { /* ignore */ }
    process.exit(0);
  };
  process.on("SIGTERM", shutdown);
  process.on("SIGINT", shutdown);
}

if (import.meta.main) {
  runCoord().catch((err) => {
    const e = err instanceof Error ? err : new Error(String(err));
    console.error(JSON.stringify({ ev: "fatal", error: e.message, stack: e.stack }));
    process.exit(1);
  });
}
