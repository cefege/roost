// Owns coordinator process boot order and lifecycle wiring for Bun listeners.
// The import-meta entrypoint calls runCoord; embedded callers may import it directly.
// It composes database, auth, transport, maintenance, and listener modules.
// Listeners must start before maintenance and signal shutdown wiring, and the
// single write gate constructed here must reach every mutation path.

import { loadCoordConfig, type CoordConfig } from "@roost/shared/config";
import { openDb } from "./db/connection.ts";
import { runMigrations } from "./db/migrate.ts";
import { CoordinatorWriteGate } from "./coordinator-write-gate.ts";
import { importAuthorizedKeys } from "./authorized-keys.ts";
import { newJwtCache } from "./jwt.ts";
import { makePreMigrationBackupHook, scheduleBackups } from "./backup.ts";
import { scheduleAuditRetention } from "./audit-retention.ts";
import { schedulePairRequestRetention } from "./pair-request-retention.ts";
import { createCoord } from "./coord-factory.ts";
import { makeWorkerWsHandler } from "./connect/worker-ws-handler.ts";
import { makeSyncWsHandler } from "./connect/sync-ws-handler.ts";
import { makeSyncTerminalControlHooks } from "./connect/sync-terminal-controls.ts";
import { TerminalViewHub, installTerminalViewHub } from "./connect/terminal-view-hub.ts";
import { COORD_GIT_SHA } from "./git-sha.ts";
import { handleWorkerUpdateProgress, resumeWindowsUpdateDeploysForWorker } from "./windows-update-deploy-jobs.ts";
import type { WorkerServiceDeps } from "./connect/worker-service.ts";
import { serveServiceHealth } from "@roost/shared/service-health";
import { log } from "@roost/shared/log";
import { ROOST_ARTIFACT_VERSION } from "@roost/shared/build-identity";
import { coordDataDir } from "@roost/shared/paths";
import { existsSync } from "node:fs";
import { randomUUID } from "node:crypto";
import { WEB_ASSETS } from "./web-embed.generated.ts";
import { createSpaResponder } from "./spa.ts";
import { MIGRATIONS } from "./migrations-embed.generated.ts";
import { runStartupJanitor } from "./startup-janitor.ts";
import { ensureSelfHostedTenant } from "./self-hosted-tenant.ts";
import { startBunCoordinatorListeners } from "./bun-coordinator-listeners.ts";
import { PendingEventPublicationStore } from "./pending-event-publications.ts";
import { UiLayoutApplyOwner } from "./connect/ui-layout-apply-owner.ts";
import { UiStateOwner } from "./connect/ui-state-owner.ts";


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
  const { db, sqlite, close: closeDb } = openDb(cfg.dbPath);
  await runMigrations(
    sqlite,
    MIGRATIONS.length > 0 ? MIGRATIONS : undefined,
    databaseExisted ? makePreMigrationBackupHook(sqlite, cfg.dbPath) : undefined,
    (name) => {
      if (name === "0024_auth_tenancy_stabilization") {
        ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
      }
    },
  );

  if (cfg.authorizedKeysPath && existsSync(cfg.authorizedKeysPath)) {
    try {
      const n = await importAuthorizedKeys(db, cfg.authorizedKeysPath);
      log.info("main", "authorized_keys_imported", { count: n, path: cfg.authorizedKeysPath });
    } catch (e) {
      log.warn("main", "authorized_keys_import_failed", { error: (e as Error).message });
    }
  }
  // One deployment shape means exactly one account/organization/dashboard, so
  // this is the only tenancy invariant and it must hold before any RPC runs.
  const selfHostedTenant = ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
  log.info("main", "db_ready", { path: cfg.dbPath });

  await runStartupJanitor(db);

  const jwtCache = newJwtCache();
  // ONE gate per process: keeper-update exclusivity is meaningless if a
  // mutation path can reach a second instance and bypass the fence.
  const writeGate = new CoordinatorWriteGate();

  const pendingPublications = new PendingEventPublicationStore();
  const uiLayoutApplies = new UiLayoutApplyOwner();
  const uiStates = new UiStateOwner();
  let closeRevokedSockets: ((fingerprint: string) => void) | null = null;
  let fenceDeletedWorker: ((fingerprint: string) => void) | null = null;
  let removeDeletedWorkerSyncScope: ((fingerprint: string) => void) | null = null;
  let closeDeletedWorkerSockets: ((fingerprint: string) => void) | null = null;
  const coord = createCoord({
    db, sqlite, cfg, jwtCache, writeGate, selfHostedTenant,
    pendingPublications, uiLayoutApplies, uiStates,
    onKeyRevoked: (fingerprint) => {
      pendingPublications.clearWorker(fingerprint);
      closeRevokedSockets?.(fingerprint);
    },
    onWorkerDeletedFence: (fingerprint) => fenceDeletedWorker?.(fingerprint),
    onWorkerDeletedSyncScope: (fingerprint) =>
      removeDeletedWorkerSyncScope?.(fingerprint),
    onWorkerDeletedSocketClose: (fingerprint) =>
      closeDeletedWorkerSockets?.(fingerprint),
  });
  const spaResponse = createSpaResponder(cfg.webDistPath, WEB_ASSETS);
  const terminalViews = new TerminalViewHub({ db });
  installTerminalViewHub(terminalViews);

  // Raw-WS worker transport deps (Bun-specific; coord-factory stays
  // fetch-only/portable). The WS handler (worker-ws-handler.ts) reuses the
  // shared worker-conn registry + makeWorkerConn from worker-service.ts.
  const wsDeps: WorkerServiceDeps = {
    db,
    pendingPublications,
    jwtCache,
    cfg,
    writeGate,
    selfHostedTenant,
    onWorkerConnected: async (workerFp) => {
      terminalViews.workerReplacement(workerFp);
      await resumeWindowsUpdateDeploysForWorker(workerFp);
    },
    onUpdateProgress: handleWorkerUpdateProgress,
  };
  const workerWs = makeWorkerWsHandler(wsDeps);
  // Sync firehose raw-WS (/ws/coord-sync) also needs SQLite for Connect deps;
  // its feed is shared with the former Connect sync via sync-feed.ts.
  const syncDeps = {
    db,
    sqlite,
    jwtCache,
    cfg,
    writeGate,
    selfHostedTenant,
    uiLayoutApplies,
    uiStates,
  };
  const syncDepsWithAccess = { ...syncDeps, cfAccess: null };
  const syncWs = makeSyncWsHandler(
    syncDepsWithAccess,
    makeSyncTerminalControlHooks(syncDepsWithAccess, terminalViews),
  );
  closeRevokedSockets = (fingerprint) => {
    terminalViews.removeFingerprint(fingerprint);
    syncWs.closeForFingerprint(fingerprint);
    workerWs.closeForFingerprint(fingerprint);
  };
  fenceDeletedWorker = (fingerprint) =>
    workerWs.fenceForFingerprint(fingerprint);
  removeDeletedWorkerSyncScope = (fingerprint) =>
    syncWs.removeWorkerFromResourceIndexes(fingerprint);
  closeDeletedWorkerSockets = (fingerprint) => {
    syncWs.closeForFingerprint(fingerprint);
    workerWs.closeForFingerprint(fingerprint);
  };

  const { server, host } = startBunCoordinatorListeners({
    cfg,
    coord,
    sqlite,
    workerDeps: wsDeps,
    syncDeps: syncDepsWithAccess,
    workerWs,
    syncWs,
    spa: spaResponse,
  });
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

  log.info("main", "listening", { bind: `${host}:${server.port}`, uptime_ms: Date.now() - bootMs });

  scheduleBackups(sqlite, cfg.dbPath);
  scheduleAuditRetention(sqlite, cfg.auditRetentionDays);
  schedulePairRequestRetention(sqlite);

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
    coord.dispose();
    installTerminalViewHub(null);
    terminalViews.dispose();
    await closeDb().catch((error) => log.warn("main", "db_close_failed", { error: String(error) }));
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
