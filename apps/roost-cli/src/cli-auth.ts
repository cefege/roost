// Owns the CLI browser identity and its authenticated transport setup.
// It never borrows worker authority: host-local enrollment redeems a one-shot
// grant, while an unknown key on any other machine requires explicit pairing.
import { Database } from "bun:sqlite";
import { Code, ConnectError } from "@connectrpc/connect";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { coordDataDir, coordServicePath } from "@roost/shared/paths";
import { supportedHostPlatform } from "@roost/shared/platform";
import type { SupportedHostPlatform } from "@roost/shared/platform";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import type { WorkerConfig } from "../../worker/src/config.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";
import type { LoadedKey } from "../../worker/src/jwt.ts";
import {
  createCoordClient,
  createUnauthenticatedCoordClient,
} from "../../worker/src/coord-client.ts";
import type { CoordClient, CoordClientOptions } from "../../worker/src/coord-client.ts";
import { mintHostBootstrapToken } from "../../coord/src/bootstrap-tokens.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { windowsServiceDefinitionsPath } from "./service-ctl.ts";

export const CLI_KEY_LABEL = "roost-cli";
export const CLI_PAIRING_REQUIRED =
  "CLI key is not enrolled; pairing required from an already enrolled browser";
export const CLI_DASHBOARD_HEADER = "x-roost-dashboard-id";
export const CLI_LEGACY_SCOPE_RESOLUTION_FAILED =
  "local CLI identity does not resolve exactly one active dashboard";

export type CliKey = LoadedKey;

export function cliKeyPath(home: string = homedir()): string {
  return join(home, ".roost", "cli-key");
}

export async function loadCliKey(): Promise<CliKey> {
  return loadWorkerKey(cliKeyPath());
}

export function cliPublicKeyB64(key: Pick<CliKey, "pubKey">): string {
  return Buffer.from(key.pubKey).toString("base64");
}

function configuredDatabasePath(
  env: Record<string, string | undefined>,
  platform: SupportedHostPlatform,
): string {
  return env.ROOST_COORDINATOR_DB ?? join(coordDataDir(env, platform), "coordinator_v2.db");
}

function databasePathFromService(
  serviceDefinition: string,
  platform: SupportedHostPlatform,
): string | null {
  if (platform === "darwin" || platform === "linux") {
    return parsePosixServiceEnvironment(serviceDefinition, platform).ROOST_COORDINATOR_DB ?? null;
  }
  try {
    const parsed = JSON.parse(serviceDefinition) as {
      services?: { coordinator?: { environment?: Record<string, unknown> } };
    };
    const value = parsed.services?.coordinator?.environment?.ROOST_COORDINATOR_DB;
    return typeof value === "string" && value.length > 0 ? value : null;
  } catch {
    return null;
  }
}

/** Resolve a local coordinator database without ever creating one. */
export function localCoordinatorDatabasePath(
  env: Record<string, string | undefined> = process.env,
  platform: SupportedHostPlatform = supportedHostPlatform(),
): string | null {
  const fallback = configuredDatabasePath(env, platform);
  const servicePath = platform === "win32"
    ? windowsServiceDefinitionsPath(env)
    : coordServicePath(env, platform);
  let candidate = fallback;
  try {
    if (existsSync(servicePath)) {
      candidate = databasePathFromService(readFileSync(servicePath, "utf8"), platform) ?? fallback;
    }
  } catch {
    candidate = fallback;
  }
  return existsSync(candidate) ? candidate : null;
}

/** Resolve the one dashboard the local CLI identity may select on legacy coords. */
export function _resolveLegacyDashboardId(
  databasePath: string,
  fingerprint: string,
): string {
  let database: Database | undefined;
  try {
    database = new Database(databasePath, { readonly: true, strict: true });
    const rows = database.query(`
      SELECT dashboard.id AS dashboard_id
      FROM account_devices AS device
      INNER JOIN accounts AS account ON account.id = device.account_id
      INNER JOIN dashboard_memberships AS dashboard_membership
        ON dashboard_membership.account_id = device.account_id
      INNER JOIN dashboards AS dashboard ON dashboard.id = dashboard_membership.dashboard_id
      INNER JOIN organizations AS organization ON organization.id = dashboard.organization_id
      INNER JOIN organization_memberships AS organization_membership
        ON organization_membership.account_id = device.account_id
       AND organization_membership.organization_id = dashboard.organization_id
      WHERE device.fingerprint = ?
        AND account.status = 'active'
        AND organization.status = 'active'
        AND dashboard.status = 'active'
        AND organization_membership.role IN ('owner', 'admin', 'member')
        AND dashboard_membership.role IN ('admin', 'member')
      ORDER BY dashboard.id
      LIMIT 2
    `).all(fingerprint) as Array<{ dashboard_id: unknown }>;
    if (
      rows.length !== 1
      || typeof rows[0]?.dashboard_id !== "string"
      || rows[0].dashboard_id.length === 0
    ) {
      throw new Error(CLI_LEGACY_SCOPE_RESOLUTION_FAILED);
    }
    return rows[0].dashboard_id;
  } catch {
    throw new Error(CLI_LEGACY_SCOPE_RESOLUTION_FAILED);
  } finally {
    database?.close(false);
  }
}

interface EnrollmentProbeClient {
  workersList(request: Record<string, never>): Promise<unknown>;
}

interface PublicEnrollmentClient {
  authRedeemBrowser(request: {
    token: string;
    sshPubkeyB64: string;
    label: string;
  }): Promise<unknown>;
}

export interface EnsureCliEnrollmentOptions {
  client: EnrollmentProbeClient;
  publicClient: PublicEnrollmentClient;
  publicKeyB64: string;
  label: string;
  localDatabasePath: string | null;
  mintHostBrowserToken?: (
    databasePath: string,
    input: { kind: "browser"; label: string },
  ) => Promise<{ token: string; expiresAtMs: number }>;
  onProtectedProbeFailure?: (
    phase: "initial" | "post-enrollment",
    error: unknown,
  ) => void;
}

function unauthenticated(error: unknown): boolean {
  return error instanceof ConnectError
    ? error.code === Code.Unauthenticated
    : /unauthenticated/i.test(String(error));
}

/**
 * Confirm the key is enrolled with a cheap protected RPC, enrolling an unknown
 * key only when the same machine owns a self-hosted coordinator database. The
 * one-shot bearer is kept in memory only and is redeemed through the public RPC.
 */
export async function ensureCliEnrollment(
  options: EnsureCliEnrollmentOptions,
): Promise<void> {
  try {
    await options.client.workersList({});
    return;
  } catch (error) {
    if (!unauthenticated(error)) {
      options.onProtectedProbeFailure?.("initial", error);
      throw error;
    }
  }

  if (!options.localDatabasePath) throw new Error(CLI_PAIRING_REQUIRED);

  const mint = options.mintHostBrowserToken ?? mintHostBootstrapToken;

  let bearer = "";
  try {
    bearer = (await mint(options.localDatabasePath, {
      kind: "browser",
      label: options.label,
    })).token;
    await options.publicClient.authRedeemBrowser({
      token: bearer,
      sshPubkeyB64: options.publicKeyB64,
      label: options.label,
    });
  } catch (error) {
    if (
      unauthenticated(error)
      || (error instanceof ConnectError && error.code === Code.PermissionDenied)
    ) {
      throw new Error(CLI_PAIRING_REQUIRED);
    }
    throw error;
  } finally {
    bearer = "";
  }

  try {
    await options.client.workersList({});
  } catch (error) {
    options.onProtectedProbeFailure?.("post-enrollment", error);
    throw error;
  }
}

export interface BuildCliClientOptions {
  coordinatorUrl?: string;
  label?: string;
  /** Tests and host tooling may force a specific local/remote classification. */
  localDatabasePath?: string | null;
}

export interface CliContext {
  client: CoordClient;
  key: CliKey;
  cfg: WorkerConfig;
  legacyDashboardId: string | null;
}

export interface BuildCliContextCredentialsOptions {
  cfg: WorkerConfig;
  key: CliKey;
  label: string;
  localDatabasePath: string | null;
  createClient?: (options: CoordClientOptions) => CoordClient;
  publicClient?: PublicEnrollmentClient;
  mintHostBrowserToken?: EnsureCliEnrollmentOptions["mintHostBrowserToken"];
  resolveLegacyDashboardId?: (databasePath: string, fingerprint: string) => string;
}

/**
 * Owns authenticated client selection after configuration and key loading.
 * Exported with an internal marker so focused tests can observe both clients.
 */
export async function _buildCliContextForCredentials(
  options: BuildCliContextCredentialsOptions,
): Promise<CliContext> {
  const createClient = options.createClient ?? createCoordClient;
  const getJwt = (): Promise<string> => mintJwt(options.key, "roost-coordinator");
  const baseClient = createClient({ cfg: options.cfg, getJwt });
  let legacyProbeFailure: ConnectError | undefined;

  try {
    await ensureCliEnrollment({
      client: baseClient,
      publicClient: options.publicClient
        ?? createUnauthenticatedCoordClient(options.cfg.coordinatorUrl),
      publicKeyB64: cliPublicKeyB64(options.key),
      label: options.label,
      localDatabasePath: options.localDatabasePath,
      ...(options.mintHostBrowserToken
        ? { mintHostBrowserToken: options.mintHostBrowserToken }
        : {}),
      onProtectedProbeFailure: (_phase, error) => {
        if (error instanceof ConnectError && error.code === Code.NotFound) {
          legacyProbeFailure = error;
        }
      },
    });
    return { client: baseClient, key: options.key, cfg: options.cfg, legacyDashboardId: null };
  } catch (error) {
    if (
      error !== legacyProbeFailure
      || !options.localDatabasePath
    ) {
      throw error;
    }
  }

  const legacyDashboardId = (options.resolveLegacyDashboardId ?? _resolveLegacyDashboardId)(
    options.localDatabasePath,
    options.key.fingerprint,
  );
  const scopedClient = createClient({
    cfg: options.cfg,
    getJwt,
    configureRequestHeaders: headers => {
      headers.set(CLI_DASHBOARD_HEADER, legacyDashboardId);
    },
  });
  await scopedClient.workersList({});
  return { client: scopedClient, key: options.key, cfg: options.cfg, legacyDashboardId };
}

/**
 * Builds an authenticated client without dashboard selection so rollout
 * preflight remains compatible with coordinators predating AuthDashboardAccess.
 */
export async function buildCliContext(
  options: BuildCliClientOptions = {},
): Promise<CliContext> {
  const cfg = loadWorkerConfig(
    options.coordinatorUrl
      ? { ROOST_COORDINATOR_URL: options.coordinatorUrl }
      : undefined,
  );
  if (!options.coordinatorUrl && process.env.ROOST_COORD_URL) {
    cfg.coordinatorUrl = process.env.ROOST_COORD_URL;
  }
  const key = await loadCliKey();
  const localDatabasePath = Object.prototype.hasOwnProperty.call(options, "localDatabasePath")
    ? options.localDatabasePath ?? null
    : localCoordinatorDatabasePath();
  return _buildCliContextForCredentials({
    cfg,
    key,
    label: options.label ?? CLI_KEY_LABEL,
    localDatabasePath,
  });
}
