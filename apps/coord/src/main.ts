// Owns coordinator process boot order and lifecycle handoff to Bun listeners.
// The import-meta entrypoint calls runCoord; embedded callers may import it directly.
// It composes database, auth, transport, move, maintenance, and listener modules.
// Listeners must start before move recovery, maintenance, and signal shutdown wiring.

import { loadCoordConfig, type CoordConfig } from "@roost/shared/config";
import { openDb } from "./db/connection.ts";
import { runMigrations } from "./db/migrate.ts";
import { loadOrCreateCoordKey } from "./coord-key.ts";
import { importAuthorizedKeys } from "./authorized-keys.ts";
import { newJwtCache } from "./jwt.ts";
import { makePreMigrationBackupHook, scheduleBackups } from "./backup.ts";
import { scheduleAuditRetention } from "./audit-retention.ts";
import { createCoord } from "./coord-factory.ts";
import { makeWorkerWsHandler } from "./connect/worker-ws-handler.ts";
import { makeSyncWsHandler } from "./connect/sync-ws-handler.ts";
import { makeSyncTerminalControlHooks } from "./connect/sync-terminal-controls.ts";
import { TerminalViewHub, installTerminalViewHub } from "./connect/terminal-view-hub.ts";
import { PasswordWorkGate } from "./connect/password-work-gate.ts";
import { prepareNativeAuthDummyHash } from "./connect/handlers-native-auth.ts";
import { CoordinatorMoveOrchestrator } from "./coord-move/orchestrator.ts";
import { HandoffStateStore } from "./coord-move/state.ts";
import { createBunCoordinatorMoveRuntime } from "./coord-move/bun-runtime.ts";
import { COORD_GIT_SHA } from "./git-sha.ts";
import { connectWorkers } from "./connect/worker-registry.ts";
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
import { createEmailDeliveryService } from "./email-delivery.ts";
import { assertManagedContainerInvariant } from "./managed-container-invariant.ts";
import { ensureSelfHostedTenant } from "./self-hosted-tenant.ts";
import { startBunCoordinatorListeners } from "./bun-coordinator-listeners.ts";
import { PendingEventPublicationStore } from "./pending-event-publications.ts";


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
  const { db, sqlite, close: closeDb } = openDb(cfg.dbPath, {
    managedContainer: cfg.managedContainer,
  });
  await runMigrations(
    sqlite,
    MIGRATIONS.length > 0 ? MIGRATIONS : undefined,
    databaseExisted
      ? makePreMigrationBackupHook(sqlite, cfg.dbPath, {
          managedContainer: cfg.managedContainer,
        })
      : undefined,
    cfg.saasMode
      ? undefined
      : (name) => {
          if (name === "0024_auth_tenancy_stabilization") {
            ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: true });
          }
        },
  );

  if (!cfg.saasMode && cfg.authorizedKeysPath && existsSync(cfg.authorizedKeysPath)) {
    try {
      const n = await importAuthorizedKeys(db, cfg.authorizedKeysPath);
      log.info("main", "authorized_keys_imported", { count: n, path: cfg.authorizedKeysPath });
    } catch (e) {
      log.warn("main", "authorized_keys_import_failed", { error: (e as Error).message });
    }
  }
  if (!cfg.saasMode) {
    ensureSelfHostedTenant(sqlite, { backfillLegacyScopes: false });
  }
  assertManagedContainerInvariant(sqlite, cfg);
  log.info("main", "db_ready", { path: cfg.dbPath });

  await runStartupJanitor(db);
  const email = cfg.resendEndpoint
    ? createEmailDeliveryService({
        db,
        resendEndpoint: cfg.resendEndpoint!,
        resendApiKey: cfg.resendApiKey!,
        emailFrom: cfg.emailFrom!,
        emailOutboxKey: cfg.emailOutboxKey!,
      })
    : undefined;
  email?.start();


  const coordKey = await loadOrCreateCoordKey(cfg.coordKeyPath);
  log.info("main", "coord_key_ready", { kid: coordKey.verifyingKeyKid() });


  const jwtCache = newJwtCache();
  const passwordWorkGate = new PasswordWorkGate();
  if (cfg.saasMode) await prepareNativeAuthDummyHash(passwordWorkGate);
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
    workers: async (dashboardId) => (await db.selectFrom("workers")
      .select(["fp", "label", "os", "git_sha", "reachable_addr"])
      .where("dashboard_id", "=", dashboardId)
      .where("deleted_at_ms", "is", null)
      .execute())
      .map((worker) => ({
        fp: worker.fp,
        label: worker.label,
        os: worker.os,
        gitSha: worker.git_sha,
        reachableAddr: worker.reachable_addr,
        online: (() => {
          const handle = connectWorkers.get(worker.fp);
          return handle?.dashboardId === dashboardId && handle.ready && !handle.revoked;
        })(),
      })),
  });
  const pendingPublications = new PendingEventPublicationStore();
  let closeRevokedSockets: ((fingerprint: string) => void) | null = null;
  let closeDashboardSockets: ((dashboardId: string, fingerprint?: string) => void) | null = null;
  let fenceDeletedWorker: ((fingerprint: string) => void) | null = null;
  let removeDeletedWorkerSyncScope:
    ((dashboardId: string, fingerprint: string) => void) | null = null;
  let closeDeletedWorkerSockets: ((fingerprint: string) => void) | null = null;
  const coord = createCoord({
    db, sqlite, coordKey, cfg, jwtCache, passwordWorkGate, move, email,
    pendingPublications,
    onKeyRevoked: (fingerprint) => {
      pendingPublications.clearWorker(fingerprint);
      closeRevokedSockets?.(fingerprint);
    },
    onWorkerDeletedFence: (fingerprint) => fenceDeletedWorker?.(fingerprint),
    onWorkerDeletedSyncScope: (dashboardId, fingerprint) =>
      removeDeletedWorkerSyncScope?.(dashboardId, fingerprint),
    onWorkerDeletedSocketClose: (fingerprint) =>
      closeDeletedWorkerSockets?.(fingerprint),
    onDashboardRevoked: (dashboardId, fingerprint) =>
      closeDashboardSockets?.(dashboardId, fingerprint),
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
    move,
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
    coordKey,
    jwtCache,
    passwordWorkGate,
    cfg,
    move,
  };
  const syncWs = makeSyncWsHandler(
    syncDeps,
    makeSyncTerminalControlHooks(syncDeps, terminalViews),
  );
  closeRevokedSockets = (fingerprint) => {
    terminalViews.removeFingerprint(fingerprint);
    syncWs.closeForFingerprint(fingerprint);
    workerWs.closeForFingerprint(fingerprint);
  };
  fenceDeletedWorker = (fingerprint) =>
    workerWs.fenceForFingerprint(fingerprint);
  removeDeletedWorkerSyncScope = (dashboardId, fingerprint) =>
    syncWs.removeWorkerFromScopes(dashboardId, fingerprint);
  closeDeletedWorkerSockets = (fingerprint) => {
    syncWs.closeForFingerprint(fingerprint);
    workerWs.closeForFingerprint(fingerprint);
  };
  closeDashboardSockets = (dashboardId, fingerprint) =>
    syncWs.closeForDashboard(dashboardId, fingerprint);
  publishRelocation = (handoffId, sourceUrl, targetUrl) => syncWs.publishRelocation(handoffId, sourceUrl, targetUrl);

  const {
    server,
    publicServer,
    host,
    tlsEnabled,
  } = startBunCoordinatorListeners({
    cfg,
    coord,
    sqlite,
    move,
    workerDeps: wsDeps,
    syncDeps,
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

  log.info("main", "listening", { bind: `${host}:${server.port}`, tls: tlsEnabled, http2: tlsEnabled, uptime_ms: Date.now() - bootMs });
  // AFTER Bun.serve: recovery stages/commits/aborts workers, and `online` is
  // computed from the worker-WS registry this server populates. Running it
  // first guarantees an empty registry, an immediate `worker offline`, and a
  // blind rollback — plus up to ~15s of delayed first byte.
  void move.recover().catch((error) => log.error("coord", "move_recover_failed", { error: String(error) }));

  scheduleBackups(sqlite, cfg.dbPath, {
    managedContainer: cfg.managedContainer,
  });
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
    await email?.stop();
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
