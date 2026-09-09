// Owns the CLI browser identity and its authenticated transport setup.
// It never borrows worker authority: host-local enrollment redeems a one-shot
// grant, while an unknown key on any other machine requires explicit pairing.
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
import type { CoordClient } from "../../worker/src/coord-client.ts";
import { mintHostBootstrapToken } from "../../coord/src/bootstrap-tokens.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { windowsServiceDefinitionsPath } from "./service-ctl.ts";

export const CLI_KEY_LABEL = "roost-cli";
export const CLI_PAIRING_REQUIRED =
  "CLI key is not enrolled; pairing required from an already enrolled browser";

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
    if (!unauthenticated(error)) throw error;
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

  await options.client.workersList({});
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
}

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
  const client = createCoordClient({
    cfg,
    getJwt: () => mintJwt(key, "roost-coordinator"),
  });
  const publicClient = createUnauthenticatedCoordClient(cfg.coordinatorUrl);
  const localDatabase = Object.prototype.hasOwnProperty.call(options, "localDatabasePath")
    ? options.localDatabasePath ?? null
    : localCoordinatorDatabasePath();
  await ensureCliEnrollment({
    client,
    publicClient,
    publicKeyB64: cliPublicKeyB64(key),
    label: options.label ?? CLI_KEY_LABEL,
    localDatabasePath: localDatabase,
  });
  return { client, key, cfg };
}

export interface CliContext {
  client: CoordClient;
  key: CliKey;
  cfg: WorkerConfig;
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
  const client = createCoordClient({
    cfg,
    getJwt: () => mintJwt(key, "roost-coordinator"),
  });
  const publicClient = createUnauthenticatedCoordClient(cfg.coordinatorUrl);
  const localDatabase = Object.prototype.hasOwnProperty.call(options, "localDatabasePath")
    ? options.localDatabasePath ?? null
    : localCoordinatorDatabasePath();
  try {
    await client.workersList({});
  } catch (error) {
    if (!unauthenticated(error)) throw error;
    if (!localDatabase) throw new Error(CLI_PAIRING_REQUIRED);
    let bearer = "";
    try {
      bearer = (await mintHostBootstrapToken(localDatabase, {
        kind: "browser",
        label: options.label ?? CLI_KEY_LABEL,
      })).token;
      await publicClient.authRedeemBrowser({
        token: bearer,
        sshPubkeyB64: cliPublicKeyB64(key),
        label: options.label ?? CLI_KEY_LABEL,
      });
    } catch (enrollmentError) {
      if (
        unauthenticated(enrollmentError)
        || (enrollmentError instanceof ConnectError && enrollmentError.code === Code.PermissionDenied)
      ) throw new Error(CLI_PAIRING_REQUIRED);
      throw enrollmentError;
    } finally {
      bearer = "";
    }
    await client.workersList({});
  }
  return { client, key, cfg };
}
