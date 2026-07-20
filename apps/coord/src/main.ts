// Coord entry point — Bun-specific wrapper. The protocol/fetch layer
// lives in coord-factory.ts (T3.1) so other runtimes can reuse it.

import { loadCoordConfig, type CoordConfig } from "@roost/shared/config";
import { openDb } from "./db/connection.ts";
import { runMigrations } from "./db/migrate.ts";
import { loadOrCreateCoordKey } from "./coord-key.ts";
import { importAuthorizedKeys } from "./authorized-keys.ts";
import { newJwtCache } from "./jwt.ts";
import { scheduleBackups } from "./backup.ts";
import { installByteHubBusHook } from "./byte-hub.ts";
import { createCoord } from "./coord-factory.ts";
import { handleWorkerWsUpgrade, makeWorkerWsHandler, type WorkerWsData } from "./connect/worker-ws-handler.ts";
import { handleSyncWsUpgrade, makeSyncWsHandler, type SyncWsData } from "./connect/sync-ws-handler.ts";
import type { WorkerServiceDeps } from "./connect/worker-service.ts";
import type { ServerWebSocket } from "bun";
import { workspaceBus } from "./buses.ts";
import { asWorkspaceId } from "@roost/shared/wire";
import { log } from "@roost/shared/log";
import { existsSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";
import { WEB_ASSETS } from "./web-embed.generated.ts";
import { MIGRATIONS } from "./migrations-embed.generated.ts";

// Text assets worth compressing on the fly. woff2/wasm/png/jpg are already
// compressed — re-encoding them wastes CPU for ~0 gain, so they stream raw.
const COMPRESSIBLE_EXT = new Set([".js", ".css", ".html", ".json", ".svg", ".map", ".txt"]);

const MIME: Record<string, string> = {
  ".html": "text/html; charset=utf-8", ".css": "text/css; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml", ".png": "image/png",
  ".jpg": "image/jpeg", ".jpeg": "image/jpeg",
  ".webp": "image/webp", ".avif": "image/avif",
  ".ico": "image/x-icon", ".wasm": "application/wasm",
  ".woff": "font/woff", ".woff2": "font/woff2", ".ttf": "font/ttf",
  ".map": "application/json; charset=utf-8",
  ".txt": "text/plain; charset=utf-8",
  ".webmanifest": "application/manifest+json",
};

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

  const coord = createCoord({ db, coordKey, cfg, jwtCache });

  // Raw-WS worker transport deps (Bun-specific; coord-factory stays
  // fetch-only/portable). The WS handler (worker-ws-handler.ts) reuses the
  // shared worker-conn registry + makeWorkerConn from worker-service.ts.
  const wsDeps: WorkerServiceDeps = { db, coordKey, jwtCache, cfg };
  const workerWs = makeWorkerWsHandler(wsDeps);
  // Sync firehose raw-WS (/ws/coord-sync). Same wsDeps shape (ConnectDeps ⊇
  // { db, coordKey, cfg, jwtCache }); the feed itself is shared with the
  // former Connect sync via startSyncFeed (handlers-streaming.ts).
  const syncWs = makeSyncWsHandler(wsDeps);

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

  // SPA static assets. Two sources, one code path: an EMBEDDED manifest
  // (compiled `roost` binary — WEB_ASSETS baked in by scripts/gen-embed.ts) or
  // the on-disk dist dir (from-source run — cfg.webDistPath). Bun.file()
  // serves a real path or an embedded-file path identically, so only the
  // candidate resolution differs. Runtimes without a filesystem provide their
  // own ctx.spa.
  const webAssets = WEB_ASSETS.size > 0 ? WEB_ASSETS : null;
  const spaRoot = !webAssets && cfg.webDistPath && existsSync(cfg.webDistPath)
    ? cfg.webDistPath.replace(/\/+$/, "")
    : null;

  // rel (no leading slash) → servable path: an embedded-file path or a disk path.
  function resolveAsset(rel: string): string | null {
    if (!rel) return null;
    if (webAssets) return webAssets.get(rel) ?? null;
    if (!spaRoot) return null;
    const candidate = join(spaRoot, rel);
    const safe = candidate === spaRoot || candidate.startsWith(spaRoot + "/");
    return safe && existsSync(candidate) && statSync(candidate).isFile() ? candidate : null;
  }
  function resolveIndex(): string | null {
    if (webAssets) return webAssets.get("index.html") ?? null;
    if (!spaRoot) return null;
    const indexPath = join(spaRoot, "index.html");
    return existsSync(indexPath) ? indexPath : null;
  }

  async function fileResponse(
    path: string, ext: string, method: string, acceptEncoding: string, isIndex: boolean, hashed: boolean,
  ): Promise<Response> {
    const headers: Record<string, string> = {
      "content-type": MIME[ext] ?? "application/octet-stream",
    };
    if (isIndex) {
      headers["cache-control"] = "no-cache, no-store, must-revalidate";
    } else if (hashed) {
      // Vite content-hashed bundle (assets/*): the filename changes on every
      // content change, so the body at this URL is genuinely immutable.
      headers["cache-control"] = "public, max-age=31536000, immutable";
    } else {
      // Stable-filename assets (icons, manifest, sw-push.js, fonts, wasm,
      // whatsnew.json): the URL is reused across builds, so it must NEVER be
      // immutable — otherwise a changed favicon/manifest stays pinned in the
      // browser for a year. Revalidate on every load instead.
      headers["cache-control"] = "no-cache";
    }
    // On-the-fly gzip via Bun's NATIVE Bun.gzipSync — NOT node:zlib (heap
    // corruption + random-later segfault under load; see git history +
    // feedback_no_connect_node_compression_under_bun). gzip only: Bun has no
    // native brotli sync, and gzip (~4.3x on the SPA chunks) is plenty.
    if (COMPRESSIBLE_EXT.has(ext) && acceptEncoding.includes("gzip")) {
      const raw = new Uint8Array(await Bun.file(path).arrayBuffer());
      headers["content-encoding"] = "gzip";
      headers["vary"] = "accept-encoding";
      return new Response(method === "HEAD" ? null : Bun.gzipSync(raw), { status: 200, headers });
    }
    return new Response(Bun.file(path), { status: 200, headers });
  }

  async function spaResponse(url: URL, method: string, acceptEncoding: string): Promise<Response> {
    if (method !== "GET" && method !== "HEAD") {
      return new Response("method not allowed", { status: 405 });
    }
    const rel = url.pathname.replace(/^\/+/, "");
    const asset = resolveAsset(rel);
    if (asset) {
      const dot = rel.lastIndexOf(".");
      return fileResponse(asset, dot >= 0 ? rel.slice(dot) : "", method, acceptEncoding, false, rel.startsWith("assets/"));
    }
    if (rel.startsWith("assets/")) {
      return new Response("not found", { status: 404 });
    }
    const index = resolveIndex();
    if (index) return fileResponse(index, ".html", method, acceptEncoding, true, false);
    return new Response("not found", { status: 404 });
  }

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
    // The worker bidi `attach` is ONE long-lived request whose body (the
    // worker→coord upstream: events + PTY bytes, incl. claude's constantly-
    // redrawing TUI) grows unbounded. Bun's default maxRequestBodySize
    // (128 MB) caps it → Bun ends the request body after ~128 MB → coord's
    // reader for-await ends → "reader_failed" → the worker reconnects. With
    // chatty claude sessions that 128 MB accrues in ~10-30s, so the worker
    // flapped on that cadence and every sessionsSpawn lost its in-flight ack
    // ("[internal] internal error"). Raise far past any realistic
    // per-connection volume so the long-lived bidi isn't body-capped.
    maxRequestBodySize: 1024 * 1024 * 1024 * 256, // 256 GiB
    async fetch(req, server) {
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

  log.info("main", "listening", { bind: cfg.bind, tls: !!tls, http2: !!tls, uptime_ms: Date.now() - bootMs });

  scheduleBackups(cfg.dbPath);

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
