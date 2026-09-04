// Status report assembly combines service state, coordinator health, worker
// inventory, installed endpoint configuration, and coordinator handoff state.
// Centralizing that I/O keeps the public command and renderer deterministic.

import { Database } from "bun:sqlite";
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { log } from "@roost/shared/log";
import { coordDataDir, coordServicePath } from "@roost/shared/paths";
import { windowsServiceDefinitionsPath } from "./service-ctl.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import {
  captureStatusCommand,
  resolveTailscale,
  statusServiceLoaded,
  STATUS_COORD_LABEL,
  STATUS_WORKER_LABEL,
} from "./status-native-probes.ts";
import type {
  HandoffStatus,
  ResolvedStatusEndpoint,
  StatusEndpointOverride,
  StatusEndpointResolverOptions,
  StatusReport,
  WorkerStatus,
} from "./status-types.ts";
import { trustedTailscaleExecutable } from "./windows/windows-identity.ts";

const WORKER_STALE_MS = 90_000;
const COORD_IDENTITY_PATH = "/roost.v1.CoordinatorService/AuthCoordIdentity";

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

function coordinatorHandoffPath(): string {
  const dataDir = process.env.ROOST_COORD_DATA_DIR ?? coordDataDir();
  return process.env.ROOST_COORDINATOR_HANDOFF_PATH
    ?? join(dataDir, "coord-handoff.json");
}

/** POST the unauthenticated coordinator identity RPC. Managed mode protects
 * MiscHealth, while identity is the shared liveness contract for every mode. */
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

/** Read the coord DB read-only for the active worker roster. Tombstones remain
 * in SQLite solely as credential and session/workspace history. */
export function workerInventory(databasePath: string = installedCoordinatorDbPath()): WorkerStatus[] {
  if (!existsSync(databasePath)) return [];
  let db: Database | null = null;
  try {
    db = new Database(databasePath, { readonly: true });
    const rows = db.query(
      `SELECT fp, label, os, reachable_addr, git_sha, keeper_stale, last_seen_ms
       FROM workers
       WHERE deleted_at_ms IS NULL`,
    ).all() as {
      fp: string;
      label: string;
      os: string;
      reachable_addr: string | null;
      git_sha: string | null;
      keeper_stale: string | null;
      last_seen_ms: number;
    }[];
    const now = Date.now();
    return rows.map((r) => {
      const ageMs = now - r.last_seen_ms;
      return {
        fingerprint: r.fp,
        label: r.label,
        os: r.os,
        reachableAddr: r.reachable_addr,
        gitSha: r.git_sha,
        keeperState: r.keeper_stale === null
          ? "unknown"
          : r.keeper_stale.length === 0 ? "current" : "stale",
        keeperBuild: r.keeper_stale && r.keeper_stale.length > 0 ? r.keeper_stale : null,
        lastSeenMs: r.last_seen_ms,
        ageMs,
        stale: ageMs > WORKER_STALE_MS,
      };
    });
  } catch (error) {
    log.warn("status", "worker_inventory_failed", { error: String(error) });
    return [];
  } finally {
    db?.close();
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
      throw new Error(`unsupported TLS service platform: ${platform}`);
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

export function resolveStatusEndpoint(
  serviceDefinition: string | null,
  options: StatusEndpointResolverOptions = {},
): ResolvedStatusEndpoint {
  const platform = options.platform ?? process.platform;
  const fronted = serviceDefinition
    ? serviceEnvironmentValue(serviceDefinition, "ROOST_FRONTED", platform)
    : null;
  const tailnetPort = serviceDefinition
    ? serviceEnvironmentValue(serviceDefinition, "ROOST_TAILNET_HTTPS_PORT", platform)
    : null;
  // Windows automatic installs terminate TLS directly; the tailnet-port marker
  // still identifies their endpoint as Tailscale-backed.
  const mode = options.override?.mode
    ?? (fronted === "1" || tailnetPort !== null ? "automatic" : "explicit");

  if (mode === "explicit") {
    const installedOrigin = serviceDefinition
      ? serviceEnvironmentValue(serviceDefinition, "ROOST_COORDINATOR_PUBLIC_URL", platform)
      : null;
    const origin = normalizeHttpsOrigin(options.override?.origin ?? installedOrigin);
    return {
      mode,
      origin,
      healthUrl: origin ? `${origin}${COORD_IDENTITY_PATH}` : null,
      tailscale: {
        required: false,
        state: "NotRequired",
        fqdn: null,
        running: false,
      },
    };
  }

  const tailscale = (options.resolveTailscale ?? resolveTailscale)();
  const configuredOrigin = options.override?.origin
    ?? (tailscale.fqdn ? `https://${tailscale.fqdn}:${tailnetPort ?? "4102"}` : null);
  const origin = normalizeHttpsOrigin(configuredOrigin);
  return {
    mode,
    origin,
    healthUrl: origin ? `${origin}${COORD_IDENTITY_PATH}` : null,
    tailscale: {
      required: true,
      state: tailscale.state,
      fqdn: tailscale.fqdn,
      running: tailscale.state === "Running",
    },
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

export function resolveTlsMode(
  serviceDefinition: string | null,
  tailscaleServeStatus: string | null,
  platform: NodeJS.Platform = process.platform,
): StatusReport["tlsMode"] {
  if (!serviceDefinition) return "missing";
  if (serviceEnvironmentValue(serviceDefinition, "ROOST_FRONTED", platform) === "1") {
    const loopbackPort = serviceEnvironmentValue(
      serviceDefinition,
      "ROOST_COORD_LOOPBACK_PORT",
      platform,
    ) ?? "4103";
    return tailscaleServeStatus?.includes(`http://127.0.0.1:${loopbackPort}`)
      ? "tailscale-serve"
      : "missing";
  }
  const cert = serviceEnvironmentValue(serviceDefinition, "ROOST_TLS_CERT_PATH", platform);
  const key = serviceEnvironmentValue(serviceDefinition, "ROOST_TLS_KEY_PATH", platform);
  return cert && key ? "direct" : "missing";
}

function currentTlsMode(
  serviceDefinition: string | null,
  tailscaleRequired: boolean,
): StatusReport["tlsMode"] {
  if (!serviceDefinition) return "missing";
  try {
    if (!tailscaleRequired
      || serviceEnvironmentValue(serviceDefinition, "ROOST_FRONTED", process.platform) !== "1") {
      return resolveTlsMode(serviceDefinition, null);
    }
    const serve = captureStatusCommand([trustedTailscaleExecutable(), "serve", "status"]);
    return resolveTlsMode(serviceDefinition, serve.exit === 0 ? serve.stdout : null);
  } catch {
    return "missing";
  }
}

/** Read coord-handoff.json (snake_case on disk). null on missing, unreadable
 *  or half-written JSON — a broken handoff file must never fail `roost status`. */
function readHandoff(): HandoffStatus | null {
  const handoffPath = coordinatorHandoffPath();
  if (!existsSync(handoffPath)) return null;
  try {
    const j = JSON.parse(readFileSync(handoffPath, "utf8")) as Record<string, unknown>;
    const { phase, handoff_id: handoffId, source_url: sourceUrl, target_url: targetUrl } = j;
    const role = j.role === "SOURCE" ? "SOURCE" : j.role === "TARGET" ? "TARGET" : null;
    if (!role) return null;
    if (typeof phase !== "string" || typeof handoffId !== "string"
      || typeof sourceUrl !== "string" || typeof targetUrl !== "string") return null;
    return { role, phase, handoffId, sourceUrl, targetUrl };
  } catch (error) {
    log.warn("status", "handoff_read_failed", { error: String(error) });
    return null;
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
  const coord = await _probeCoordinatorIdentity(endpoint.healthUrl);
  return {
    tailscale: endpoint.tailscale,
    coordAgentLoaded: statusServiceLoaded(STATUS_COORD_LABEL),
    workerAgentLoaded: statusServiceLoaded(STATUS_WORKER_LABEL),
    coord,
    workers: workerInventory(),
    tlsMode: currentTlsMode(serviceDefinition, endpoint.tailscale.required),
    url: endpoint.origin,
    handoff: readHandoff(),
  };
}
