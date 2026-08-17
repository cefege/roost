// Coord entry point — Bun-specific wrapper. The protocol/fetch layer
// lives in coord-factory.ts (T3.1) so other runtimes can reuse it.

import { loadCoordConfig, type CoordConfig } from "@roost/shared/config";
import { openDb } from "./db/connection.ts";
import { runMigrations } from "./db/migrate.ts";
import { loadOrCreateCoordKey } from "./coord-key.ts";
import { importAuthorizedKeys } from "./authorized-keys.ts";
import { newJwtCache } from "./jwt.ts";
import { runBackup, scheduleBackups } from "./backup.ts";
import { scheduleAuditRetention } from "./audit-retention.ts";
import { installByteHubBusHook } from "./byte-hub.ts";
import { createCoord } from "./coord-factory.ts";
import { handleWorkerWsUpgrade, makeWorkerWsHandler, type WorkerWsData } from "./connect/worker-ws-handler.ts";
import { handleSyncWsUpgrade, makeSyncWsHandler, type SyncWsData } from "./connect/sync-ws-handler.ts";
import { makeSyncTerminalControlHooks } from "./connect/sync-terminal-controls.ts";
import { CoordinatorMoveOrchestrator } from "./coord-move/orchestrator.ts";
import { HandoffStateStore } from "./coord-move/state.ts";
import { createBunCoordinatorMoveRuntime } from "./coord-move/bun-runtime.ts";
import { handleInternalHandoffRequest } from "./coord-move/internal-http.ts";
import { COORD_GIT_SHA } from "./git-sha.ts";
import { connectWorkers } from "./connect/worker-registry.ts";
import { handleWorkerUpdateProgress, resumeWindowsUpdateDeploysForWorker } from "./deploy-jobs.ts";
import type { WorkerServiceDeps } from "./connect/worker-service.ts";
import type { Server, ServerWebSocket } from "bun";
import { workspaceBus } from "./buses.ts";
import { asWorkspaceId } from "@roost/shared/wire";
import { serveServiceHealth } from "@roost/shared/service-health";
import { log } from "@roost/shared/log";
import { ROOST_ARTIFACT_VERSION } from "@roost/shared/build-identity";
import { coordDataDir } from "@roost/shared/paths";
import { existsSync, readFileSync, rmSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { dirname, join } from "node:path";
import { WEB_ASSETS } from "./web-embed.generated.ts";
import { createSpaResponder } from "./spa.ts";
import { MIGRATIONS } from "./migrations-embed.generated.ts";
import { createSqliteSnapshot } from "./db/snapshot.ts";
import {
  resolveCallerOrigin,
  type CallerOrigin,
  type ListenerTrust,
} from "./middleware/caller-origin.ts";
import { makeCfAccessVerifier } from "./middleware/cf-access.ts";
import { coordinatorAvailabilityResponse } from "./middleware/coordinator-availability.ts";
import { makePublicSurface } from "./middleware/public-surface.ts";


export async function runCoord() {
  const bootMs = Date.now();
  const processEpoch = randomUUID();

  let cfg: CoordConfig;
  try {
    cfg = loadCoordConfig(process.env as Record<string, string | undefined>);
  } catch (e) {
    console.error(JSON.stringify({ ev: "config_error", error: (e as Error).message }));
    process.exit(1);
  }

  const databaseExisted = existsSync(cfg.dbPath);
  const { db, sqlite } = openDb(cfg.dbPath);
  await runMigrations(
    sqlite,
    MIGRATIONS.length > 0 ? MIGRATIONS : undefined,
    databaseExisted
      ? async () => {
          await runBackup(sqlite, cfg.dbPath, "pre-migration");
        }
      : undefined,
  );
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
  let closeRevokedSockets: ((fingerprint: string) => void) | null = null;
  const coord = createCoord({
    db, sqlite, coordKey, cfg, jwtCache, move,
    onKeyRevoked: (fingerprint) => closeRevokedSockets?.(fingerprint),
  });
  const spaResponse = createSpaResponder(cfg.webDistPath, WEB_ASSETS);

  // Raw-WS worker transport deps (Bun-specific; coord-factory stays
  // fetch-only/portable). The WS handler (worker-ws-handler.ts) reuses the
  // shared worker-conn registry + makeWorkerConn from worker-service.ts.
  const wsDeps: WorkerServiceDeps = {
    db,
    coordKey,
    jwtCache,
    cfg,
    move,
    onWorkerConnected: resumeWindowsUpdateDeploysForWorker,
    onUpdateProgress: handleWorkerUpdateProgress,
  };
  const workerWs = makeWorkerWsHandler(wsDeps);
  // Sync firehose raw-WS (/ws/coord-sync) also needs SQLite for Connect deps;
  // its feed is shared with the former Connect sync via handlers-streaming.ts.
  const syncDeps = { db, sqlite, coordKey, jwtCache, cfg, move };
  const syncWs = makeSyncWsHandler(
    syncDeps,
    makeSyncTerminalControlHooks(syncDeps),
  );
  closeRevokedSockets = (fingerprint) => {
    syncWs.closeForFingerprint(fingerprint);
    workerWs.closeForFingerprint(fingerprint);
  };
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
    drain(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.drain(ws as ServerWebSocket<SyncWsData>);
    },
    close(ws: ServerWebSocket<WorkerWsData | SyncWsData>): void {
      if (ws.data.kind === "sync") syncWs.close(ws as ServerWebSocket<SyncWsData>);
      else workerWs.close(ws as ServerWebSocket<WorkerWsData>);
    },
  };
  const publicSurface = cfg.publicBind
    ? makePublicSurface({
        access: makeCfAccessVerifier(cfg.cfAccessTeamDomain!, cfg.cfAccessAud!),
        coord,
        syncDeps,
        move,
        cfg,
        spa: spaResponse,
        syncUpgrade: handleSyncWsUpgrade,
      })
    : null;


  async function dbExportResponse(origin: CallerOrigin): Promise<Response> {
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
      const unavailable = coordinatorAvailabilityResponse(move.gate.mode, req.method, path);
      if (unavailable) return unavailable;
      // Worker raw-WS transport (/ws/coord-worker/:fp). If this is that
      // upgrade, authenticate + hijack here (Bun-specific); null = not our
      // path → fall through to the portable coord.fetch.
      const ws = await handleWorkerWsUpgrade(req, server, wsDeps);
      if (ws !== null) return ws;
      // Browser Sync firehose raw-WS (/ws/coord-sync). Same hijack pattern;
      // null = not our path → fall through to the portable coord.fetch.
      const sws = await handleSyncWsUpgrade(req, server, syncDeps);
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
        server.requestIP(req)?.address,
        req.headers,
      );
      return coord.fetch(req, {
        origin,
        spa: spaResponse,
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
      access_team: cfg.cfAccessTeamDomain,
    });
  }
  let closeServiceHealth: (() => Promise<void>) | undefined;
  switch (process.platform) {
    case "win32": {
      const health = await serveServiceHealth("coordinator", () => ({
        role: "coordinator",
        version: ROOST_ARTIFACT_VERSION === "dev" ? COORD_GIT_SHA : ROOST_ARTIFACT_VERSION,
        build: COORD_GIT_SHA,
        processEpoch,
        ready: true,
        dbReady: true,
        listenerReady: true,
        advertisedUrl: cfg.publicUrl ?? `https://${host}:${server.port}`,
      }), { dataDir: coordDataDir() });
      closeServiceHealth = () => health.close();
      break;
    }
    case "darwin":
    case "linux":
      break;
    default:
      throw new Error(`unsupported coordinator platform: ${process.platform}`);
  }

  log.info("main", "listening", { bind: `${host}:${server.port}`, tls: !!tls, http2: !!tls, uptime_ms: Date.now() - bootMs });
  // AFTER Bun.serve: recovery stages/commits/aborts workers, and `online` is
  // computed from the worker-WS registry this server populates. Running it
  // first guarantees an empty registry, an immediate `worker offline`, and a
  // blind rollback — plus up to ~15s of delayed first byte.
  void move.recover().catch((error) => log.error("coord", "move_recover_failed", { error: String(error) }));

  scheduleBackups(sqlite, cfg.dbPath);
  scheduleAuditRetention(sqlite, cfg.auditRetentionDays);

  let shuttingDown = false;
  const shutdown = async (): Promise<void> => {
    if (shuttingDown) return;
    shuttingDown = true;
    log.info("main", "shutdown");
    try {
      await closeServiceHealth?.();
    } catch (error) {
      log.warn("main", "service_health_close_failed", { error: String(error) });
    }
    server.stop(true);
    publicServer?.stop(true);
    coord.dispose();
    try { sqlite.close(); } catch { /* ignore */ }
    process.exit(0);
  };
  process.on("SIGTERM", () => { void shutdown(); });
  process.on("SIGINT", () => { void shutdown(); });
}

if (import.meta.main) {
  runCoord().catch((err) => {
    const e = err instanceof Error ? err : new Error(String(err));
    console.error(JSON.stringify({ ev: "fatal", error: e.message, stack: e.stack }));
    process.exit(1);
  });
}
