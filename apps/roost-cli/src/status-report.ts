// Status report assembly combines service state, coordinator liveness, the
// declared front door, and worker inventory. Centralizing that I/O keeps the
// public command and renderer deterministic.

import { Database } from "bun:sqlite";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { log } from "@roost/shared/log";
import {
  KeeperRuntimeObservationV1Schema,
  type KeeperRuntimeObservationV1,
} from "@roost/shared/keeper-update";
import {
  TerminalCoreCapacityReportSchema,
  type TerminalCoreCapacityReport,
} from "@roost/shared/terminal-core-capacity";
import { coordDataDir, coordServicePath } from "@roost/shared/paths";
import { windowsServiceDefinitionsPath } from "./service-ctl.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  statusServiceLoaded,
  STATUS_COORD_LABEL,
  STATUS_WORKER_LABEL,
} from "./status-native-probes.ts";
import type {
  ResolvedStatusEndpoint,
  StatusEndpointOverride,
  StatusEndpointResolverOptions,
  StatusReport,
  WorkerStatus,
} from "./status-types.ts";

const WORKER_STALE_MS = 90_000;
const COORD_IDENTITY_PATH = "/roost.v1.CoordinatorService/AuthCoordIdentity";

function identityUrl(origin: string | null): string | null {
  return origin ? `${origin}${COORD_IDENTITY_PATH}` : null;
}

function defaultCoordinatorDbPath(): string {
  const dataDir = process.env.ROOST_COORD_DATA_DIR ?? coordDataDir();
  return process.env.ROOST_COORDINATOR_DB
    ?? join(dataDir, "coordinator_v2.db");
}

function coordinatorServiceFile(): string {
  return process.platform === "win32"
    ? windowsServiceDefinitionsPath()
    : coordServicePath();
}

/** POST the unauthenticated coordinator identity RPC: the liveness contract
 * every listener answers, whether reached directly or through a front door. */
export async function _probeCoordinatorIdentity(
  healthUrl: string | null,
  fetchImpl: typeof fetch = fetch,
): Promise<{ reachable: boolean; gitSha: string | null }> {
  if (!healthUrl) return { reachable: false, gitSha: null };
  try {
    const response = await fetchImpl(healthUrl, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
      signal: AbortSignal.timeout(5000),
    });
    if (!response.ok) return { reachable: false, gitSha: null };
    const identity = (await response.json()) as { gitSha?: unknown };
    if (typeof identity.gitSha !== "string" || identity.gitSha.length === 0) {
      return { reachable: false, gitSha: null };
    }
    return { reachable: true, gitSha: identity.gitSha };
  } catch {
    return { reachable: false, gitSha: null };
  }
}
export function parseKeeperRuntimeJson(
  serialized: string | null,
): KeeperRuntimeObservationV1 | null {
  if (!serialized) return null;
  try {
    const parsed = KeeperRuntimeObservationV1Schema.safeParse(
      JSON.parse(serialized),
    );
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}

export function parseTerminalCoreCapacityJson(
  serialized: string | null,
): TerminalCoreCapacityReport | null {
  if (!serialized) return null;
  try {
    const parsed = TerminalCoreCapacityReportSchema.safeParse(
      JSON.parse(serialized),
    );
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}


interface WorkerInventoryRow {
  fp: string;
  label: string;
  os: string;
  reachable_addr: string | null;
  git_sha: string | null;
  keeper_runtime_json: string | null;
  terminal_core_capacity_json: string | null;
  last_seen_ms: number;
}

function readWorkerInventorySnapshot(db: Database): WorkerStatus[] {
  return db.transaction(() => {
    const keeperRuntimeColumn = db.query(
      `SELECT name FROM pragma_table_info('workers')
       WHERE name = 'keeper_runtime_json'`,
    ).get();
    const keeperRuntimeProjection = keeperRuntimeColumn
      ? "keeper_runtime_json"
      : "NULL AS keeper_runtime_json";
    const terminalCoreCapacityColumn = db.query(
      `SELECT name FROM pragma_table_info('workers')
       WHERE name = 'terminal_core_capacity_json'`,
    ).get();
    const terminalCoreCapacityProjection = terminalCoreCapacityColumn
      ? "terminal_core_capacity_json"
      : "NULL AS terminal_core_capacity_json";
    const rows = db.query(
      `SELECT fp, label, os, reachable_addr, git_sha,
              ${keeperRuntimeProjection}, ${terminalCoreCapacityProjection},
              last_seen_ms
       FROM workers
       WHERE deleted_at_ms IS NULL`,
    ).all() as WorkerInventoryRow[];
    const sessions = db.query(
      `SELECT id, worker_fp
       FROM sessions
       WHERE status = 'open'`,
    ).all() as Array<{ id: string; worker_fp: string }>;
    const openSessionIdsByWorker = new Map<string, string[]>();
    for (const session of sessions) {
      const workerSessions = openSessionIdsByWorker.get(session.worker_fp) ?? [];
      workerSessions.push(session.id);
      openSessionIdsByWorker.set(session.worker_fp, workerSessions);
    }
    const now = Date.now();
    return rows.map((row) => {
      const ageMs = now - row.last_seen_ms;
      return {
        fingerprint: row.fp,
        label: row.label,
        os: row.os,
        reachableAddr: row.reachable_addr,
        gitSha: row.git_sha,
        keeperRuntime: parseKeeperRuntimeJson(row.keeper_runtime_json),
        terminalCoreCapacity: parseTerminalCoreCapacityJson(
          row.terminal_core_capacity_json,
        ),
        coordinatorOpenSessionIds: (
          openSessionIdsByWorker.get(row.fp) ?? []
        ).sort(),
        lastSeenMs: row.last_seen_ms,
        ageMs,
        stale: ageMs > WORKER_STALE_MS,
      };
    });
  })();
}

/** Read one admission snapshot or throw when the coordinator database cannot
 * prove both its active workers and their open sessions. */
export function workerInventoryForUpdateAdmission(
  databasePath: string = installedCoordinatorDbPath(),
): WorkerStatus[] {
  if (!existsSync(databasePath)) {
    throw new Error(`coordinator database not found: ${databasePath}`);
  }
  const db = new Database(databasePath, { readonly: true });
  try {
    return readWorkerInventorySnapshot(db);
  } finally {
    db.close();
  }
}


/** Read the coord DB read-only for the status display. Missing or incompatible
 * databases render as an empty roster rather than failing the whole command. */
export function workerInventory(
  databasePath: string = installedCoordinatorDbPath(),
): WorkerStatus[] {
  if (!existsSync(databasePath)) return [];
  try {
    return workerInventoryForUpdateAdmission(databasePath);
  } catch (error) {
    log.warn("status", "worker_inventory_failed", { error: String(error) });
    return [];
  }
}

function serviceEnvironmentValue(
  serviceDefinition: string,
  name: string,
  platform: NodeJS.Platform,
): string | null {
  switch (platform) {
    case "darwin":
    case "linux":
      return parsePosixServiceEnvironment(serviceDefinition, platform)[name] ?? null;
    case "win32": {
      try {
        const stored = JSON.parse(serviceDefinition) as {
          services?: { coordinator?: { environment?: Record<string, unknown> } };
        };
        const value = stored.services?.coordinator?.environment?.[name];
        return typeof value === "string" ? value : null;
      } catch {
        return null;
      }
    }
    default:
      throw new Error(`unsupported coordinator service platform: ${platform}`);
  }
}

function normalizeHttpsOrigin(value: string | null): string | null {
  if (!value) return null;
  try {
    const url = new URL(value);
    return url.protocol === "https:" ? url.origin : null;
  } catch {
    return null;
  }
}

/** The two origins `roost status` can speak about: the operator's declared
 * front door, and the coordinator's own loopback listener as the installed
 * service definition binds it. */
export function resolveStatusEndpoint(
  serviceDefinition: string | null,
  options: StatusEndpointResolverOptions = {},
): ResolvedStatusEndpoint {
  const platform = options.platform ?? process.platform;
  // Installed units carry declared-but-empty entries
  // (Environment="ROOST_COORDINATOR_PUBLIC_URL="); those declare nothing.
  const installedValue = (name: string): string | null => {
    const declared = serviceDefinition
      ? serviceEnvironmentValue(serviceDefinition, name, platform)?.trim()
      : null;
    return declared ? declared : null;
  };
  const declared = options.override?.origin
    ?? installedValue("ROOST_WEB_PUBLIC_URL")
    ?? installedValue("ROOST_COORDINATOR_PUBLIC_URL");
  const bind = installedValue("ROOST_COORDINATOR_BIND");
  return {
    publicUrl: normalizeHttpsOrigin(declared),
    coordUrl: bind ? `http://${bind}` : null,
  };
}

export function resolveCoordinatorDbPath(
  serviceDefinition: string | null,
  platform: NodeJS.Platform = process.platform,
  fallback: string = defaultCoordinatorDbPath(),
): string {
  if (!serviceDefinition) return fallback;
  const installed = serviceEnvironmentValue(serviceDefinition, "ROOST_COORDINATOR_DB", platform);
  return installed ? installed : fallback;
}

function installedCoordinatorDbPath(): string {
  const serviceFile = coordinatorServiceFile();
  if (!existsSync(serviceFile)) return defaultCoordinatorDbPath();
  try {
    return resolveCoordinatorDbPath(
      readFileSync(serviceFile, "utf8"),
      process.platform,
      defaultCoordinatorDbPath(),
    );
  } catch {
    return defaultCoordinatorDbPath();
  }
}

export async function statusReport(
  endpointOverride?: StatusEndpointOverride,
): Promise<StatusReport> {
  let serviceDefinition: string | null = null;
  const serviceFile = coordinatorServiceFile();
  try {
    if (existsSync(serviceFile)) serviceDefinition = readFileSync(serviceFile, "utf8");
  } catch { /* status remains available with a damaged definition */ }
  const endpoint = resolveStatusEndpoint(serviceDefinition, { override: endpointOverride });
  // Liveness is the coordinator's own listener, so a front door the operator
  // has not finished wiring never reads as a dead coordinator. Off a
  // coordinator host there is no bind to probe, so the front door is all there
  // is to ask.
  const coordUrl = endpoint.coordUrl ?? endpoint.publicUrl;
  const coord = await _probeCoordinatorIdentity(identityUrl(coordUrl));
  return {
    coordAgentLoaded: statusServiceLoaded(STATUS_COORD_LABEL),
    workerAgentLoaded: statusServiceLoaded(STATUS_WORKER_LABEL),
    coord,
    workers: workerInventory(),
    endpoint: {
      publicUrl: endpoint.publicUrl,
      answers: endpoint.publicUrl === null
        ? false
        : endpoint.publicUrl === coordUrl
          ? coord.reachable
          : (await _probeCoordinatorIdentity(identityUrl(endpoint.publicUrl))).reachable,
    },
  };
}
