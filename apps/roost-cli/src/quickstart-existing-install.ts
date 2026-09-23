// Existing quickstart transition owns validated service discovery, coordinator
// reactivation, narrow front-door promotion, and worker identity preservation.
// quickstart.ts supplies only first-worker provisioning; this module never replaces a live worker.
import { existsSync, mkdtempSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { durableWriteFile } from "@roost/shared/durability";
import { coordServicePath, workerServicePath } from "@roost/shared/paths";
import { loadCoordConfig } from "@roost/shared/config";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { createCoordClient } from "../../worker/src/coord-client.ts";
import { runStrictEnrollmentWithKey } from "../../worker/src/install.ts";
import { mintJwt, readExistingWorkerKey, readWorkerFingerprint } from "../../worker/src/jwt.ts";
import {
  coordinatorInstallEnvironment,
  coordinatorRestartCommand,
  coordinatorServiceWithEndpoint,
} from "./coordinator-service-definition.ts";
import { parsePosixServiceEnvironment } from "./deploy-plist-env.ts";
import { run } from "./deploy-exec.ts";
import { acquireMachineTransaction } from "./machine-transaction.ts";
import {
  isRegisteredWorker,
  mintWorkerToken,
} from "./quickstart-bootstrap-tokens.ts";
import {
  quickstartLoopbackOrigin,
  resolveQuickstartEndpoint,
} from "./quickstart-endpoint.ts";
import type { QuickstartEndpoint, QuickstartOptions } from "./quickstart-endpoint.ts";
import {
  waitForCoordHealth,
  waitForCoordSpa,
  waitForWorkerRegistration,
  waitForWorkerRoutability,
} from "./quickstart-runtime.ts";
import { startWorkerCmd } from "./service-posix.ts";
import { captureStatusCommand } from "./status-native-probes.ts";
export type PosixPlatform = "darwin" | "linux";
type WorkerLifecycle = "running" | "stopped" | "unloaded";
export interface ExistingQuickstartInstall {
  platform: PosixPlatform;
  servicePath: string;
  serviceLabel: string;
  definition: string;
  environment: Record<string, string>;
}
export interface ExistingWorkerInstall {
  servicePath: string;
  serviceLabel: string;
  environment: Record<string, string>;
  fingerprint: string;
}
export interface ExistingQuickstartProvisioner {
  (input: { coordinatorUrl: string; bootstrapToken: string }): Promise<void>;
}
export interface ExistingQuickstartResult {
  endpoint: QuickstartEndpoint;
  databasePath: string;
  workerFingerprint: string;
  remoteAccessVerified: boolean | null;
}
function platformOrThrow(platform: NodeJS.Platform): PosixPlatform {
  if (platform === "darwin" || platform === "linux") return platform;
  throw new Error(`existing quickstart is unsupported on ${platform}`);
}

function xmlUnescape(value: string): string {
  return value
    .replaceAll("&lt;", "<")
    .replaceAll("&gt;", ">")
    .replaceAll("&quot;", "\"")
    .replaceAll("&apos;", "'")
    .replaceAll("&amp;", "&");
}

function serviceLabelFromDefinition(
  definition: string,
  servicePath: string,
  platform: PosixPlatform,
  role: "coordinator" | "worker",
): string {
  if (platform === "linux") {
    const label = basename(servicePath, ".service");
    if (!label) throw new Error(`installed ${role} service has no systemd label: ${servicePath}`);
    return label;
  }
  const match = /<key>Label<\/key>\s*<string>([^<]+)<\/string>/.exec(definition);
  const label = match ? xmlUnescape(match[1]!) : "";
  if (!label) throw new Error(`installed ${role} service has no launchd label: ${servicePath}`);
  return label;
}

function assertUnambiguousEnvironment(definition: string, platform: PosixPlatform, servicePath: string): void {
  const seen = new Set<string>();
  const entries = platform === "darwin"
    ? definition.matchAll(/<key>(ROOST_[A-Z_]+|GIT_SHA)<\/key>\s*<string>[^<]*<\/string>/g)
    : definition.matchAll(/^Environment=(?:"(ROOST_[A-Z_]+|GIT_SHA)=(?:\\.|[^"])*"|(ROOST_[A-Z_]+|GIT_SHA)=[^\r\n]*)$/gm);
  for (const match of entries) {
    const key = match[1] ?? match[2];
    if (!key) continue;
    if (seen.has(key)) throw new Error(`installed service environment is ambiguous for ${key}: ${servicePath}`);
    seen.add(key);
  }
}

function validateCoordinatorDefinition(
  definition: string,
  platform: PosixPlatform,
  servicePath: string,
): Record<string, string> {
  assertUnambiguousEnvironment(definition, platform, servicePath);
  const environment = coordinatorInstallEnvironment(definition, platform);
  const bind = environment.ROOST_COORDINATOR_BIND;
  if (!bind || !/^127\.0\.0\.1:([1-9]\d{0,4})$/.test(bind)) {
    throw new Error(`installed ROOST_COORDINATOR_BIND must be 127.0.0.1:<port>: ${servicePath}`);
  }
  if (Number(bind.slice(bind.lastIndexOf(":") + 1)) > 65535) {
    throw new Error(`installed ROOST_COORDINATOR_BIND port must be 1-65535: ${servicePath}`);
  }
  if (!environment.ROOST_COORDINATOR_DB) {
    throw new Error(`installed coordinator service does not declare ROOST_COORDINATOR_DB: ${servicePath}`);
  }
  const trustProxy = environment.ROOST_TRUST_PROXY;
  if (trustProxy !== undefined && trustProxy !== "0" && trustProxy !== "1") {
    throw new Error(`installed ROOST_TRUST_PROXY must be exactly 0 or 1: ${servicePath}`);
  }
  loadCoordConfig(environment);
  return environment;
}

export function discoverExistingQuickstartInstall(
  platform: NodeJS.Platform = process.platform,
): ExistingQuickstartInstall | null {
  const posixPlatform = platformOrThrow(platform);
  const servicePath = coordServicePath(process.env, posixPlatform);
  if (!existsSync(servicePath)) return null;
  let definition: string;
  try {
    definition = readFileSync(servicePath, "utf8");
  } catch (error) {
    throw new Error(`cannot read installed coordinator service ${servicePath}: ${String(error)}`);
  }
  return {
    platform: posixPlatform,
    servicePath,
    serviceLabel: serviceLabelFromDefinition(definition, servicePath, posixPlatform, "coordinator"),
    definition,
    environment: validateCoordinatorDefinition(definition, posixPlatform, servicePath),
  };
}

export async function _discoverExistingWorker(
  platform: PosixPlatform,
): Promise<ExistingWorkerInstall | null> {
  const servicePath = workerServicePath(process.env, platform);
  if (!existsSync(servicePath)) return null;
  let definition: string;
  try {
    definition = readFileSync(servicePath, "utf8");
  } catch (error) {
    throw new Error(`cannot read installed worker service ${servicePath}: ${String(error)}`);
  }
  assertUnambiguousEnvironment(definition, platform, servicePath);
  const environment = parsePosixServiceEnvironment(definition, platform);
  if (!environment.ROOST_COORDINATOR_URL) {
    throw new Error(`installed worker service does not declare ROOST_COORDINATOR_URL: ${servicePath}`);
  }
  const config = loadWorkerConfig(environment, platform);
  let fingerprint: string;
  try {
    fingerprint = await readWorkerFingerprint(config.workerKeyPath);
  } catch (error) {
    throw new Error(`installed worker key needs explicit repair (${config.workerKeyPath}): ${String(error)}`);
  }
  return {
    servicePath,
    serviceLabel: serviceLabelFromDefinition(definition, servicePath, platform, "worker"),
    environment,
    fingerprint,
  };
}

function workerLifecycle(worker: ExistingWorkerInstall, platform: PosixPlatform): WorkerLifecycle {
  if (platform === "linux") {
    const status = captureStatusCommand([
      "systemctl",
      "--user",
      "show",
      `${worker.serviceLabel}.service`,
      "--property=ActiveState",
      "--property=MainPID",
      "--property=LoadState",
    ]);
    if (status.exit !== 0) {
      if (/^LoadState=not-found$/m.test(status.stdout)) return "unloaded";
      throw new Error(`cannot capture installed worker lifecycle: ${worker.servicePath}`);
    }
    const activeState = /^ActiveState=(.+)$/m.exec(status.stdout)?.[1];
    const mainPid = /^MainPID=(\d+)$/m.exec(status.stdout)?.[1];
    if (!activeState || mainPid === undefined) {
      throw new Error(`installed worker lifecycle probe was incomplete: ${worker.servicePath}`);
    }
    if (activeState === "active" && Number(mainPid) > 0) return "running";
    if (["inactive", "failed"].includes(activeState) && Number(mainPid) === 0) return "stopped";
    throw new Error(`installed worker lifecycle is not safely startable: ${worker.servicePath}`);
  }
  const uid = process.getuid?.();
  if (uid === undefined) throw new Error("cannot determine launchd user id");
  const status = captureStatusCommand(["launchctl", "print", `gui/${uid}/${worker.serviceLabel}`]);
  if (status.exit === 113) return "unloaded";
  if (status.exit !== 0) throw new Error(`cannot capture installed worker lifecycle: ${worker.servicePath}`);
  const pid = /\bpid\s*=\s*(\d+)/.exec(status.stdout)?.[1];
  return pid && Number(pid) > 0 ? "running" : "stopped";
}

async function enrollExistingWorker(
  worker: ExistingWorkerInstall,
  platform: PosixPlatform,
  bootstrapToken: string,
): Promise<void> {
  const config = loadWorkerConfig({ ...worker.environment, ROOST_BOOTSTRAP_TOKEN: bootstrapToken }, platform);
  const key = await readExistingWorkerKey(config.workerKeyPath);
  if (key.fingerprint !== worker.fingerprint) throw new Error("installed worker key changed during enrollment; explicit repair is required");
  const client = createCoordClient({
    cfg: config,
    getJwt: () => mintJwt(key, "roost-coordinator"),
  });
  const priorServicePath = process.env.ROOST_WORKER_SERVICE_PATH;
  process.env.ROOST_WORKER_SERVICE_PATH = worker.environment.ROOST_WORKER_SERVICE_PATH ?? worker.servicePath;
  try {
    await runStrictEnrollmentWithKey({ cfg: config, client }, key);
  } finally {
    if (priorServicePath === undefined) delete process.env.ROOST_WORKER_SERVICE_PATH;
    else process.env.ROOST_WORKER_SERVICE_PATH = priorServicePath;
  }
}
async function restartCoordinator(
  installed: ExistingQuickstartInstall,
  endpoint: QuickstartEndpoint,
): Promise<void> {
  const command = coordinatorRestartCommand(installed.servicePath, installed.platform, installed.serviceLabel);
  const restarted = await run(["bash", "-lc", command], { quiet: true });
  if (restarted.exit !== 0) {
    throw new Error(`coordinator activation failed (${restarted.exit}): ${restarted.stderr || restarted.stdout}`);
  }
  if (!await waitForCoordHealth(endpoint)) {
    throw new Error("coord did not become healthy on its loopback bind after activation");
  }
  if (!await waitForCoordSpa(endpoint)) {
    throw new Error("coord became healthy but did not serve the SPA from its loopback root");
  }
}

export async function _reactivateCoordinator(
  discovered: ExistingQuickstartInstall,
  endpoint: QuickstartEndpoint,
  promotion: boolean,
): Promise<ExistingQuickstartInstall> {
  const transaction = await acquireMachineTransaction("install", discovered.servicePath, {
    platform: discovered.platform,
  });
  try {
    const current = discoverExistingQuickstartInstall(discovered.platform);
    if (!current) throw new Error(`installed coordinator service disappeared: ${discovered.servicePath}`);
    const resolvedEndpoint = resolveQuickstartEndpoint([], current.platform, current.environment);
    const effectiveEndpoint = promotion
      ? resolveQuickstartEndpoint(["--coordinator-url", endpoint.webPublicUrl!], current.platform, current.environment)
      : resolvedEndpoint;
    const candidate = promotion
      ? coordinatorServiceWithEndpoint(current.definition, current.platform, effectiveEndpoint)
      : current.definition;
    validateCoordinatorDefinition(candidate, current.platform, current.servicePath);
    const changed = candidate !== current.definition;
    const priorMode = statSync(current.servicePath).mode & 0o777;
    try {
      if (changed) await durableWriteFile(current.servicePath, candidate, { platform: current.platform, mode: priorMode });
      await restartCoordinator(current, effectiveEndpoint);
    } catch (error) {
      if (!changed) throw error;
      const restorationErrors: string[] = [];
      try {
        await durableWriteFile(current.servicePath, current.definition, { platform: current.platform, mode: priorMode });
      } catch (restoreError) {
        restorationErrors.push(`definition restore failed: ${String(restoreError)}`);
      }
      try {
        await restartCoordinator(current, resolvedEndpoint);
      } catch (restoreError) {
        restorationErrors.push(`prior coordinator restart failed: ${String(restoreError)}`);
      }
      throw new Error([
        `coordinator promotion failed: ${String(error)}`,
        ...restorationErrors,
      ].join("; "));
    }
    return {
      ...current,
      definition: candidate,
      environment: validateCoordinatorDefinition(candidate, current.platform, current.servicePath),
    };
  } finally {
    await transaction.release();
  }
}

async function remoteHealth(endpoint: QuickstartEndpoint): Promise<boolean> {
  if (!endpoint.webPublicUrl) return false;
  try {
    const response = await fetch(`${endpoint.webPublicUrl}/roost.v1.CoordinatorService/MiscHealth`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
      signal: AbortSignal.timeout(3_000),
    });
    const body: unknown = await response.json();
    return response.ok
      && typeof body === "object"
      && body !== null
      && "ok" in body
      && body.ok === true;
  } catch {
    return false;
  }
}

export async function dryRunExistingQuickstart(
  installed: ExistingQuickstartInstall,
  endpoint: QuickstartEndpoint,
  promotion: boolean,
): Promise<void> {
  const candidate = promotion
    ? coordinatorServiceWithEndpoint(installed.definition, installed.platform, endpoint)
    : installed.definition;
  validateCoordinatorDefinition(candidate, installed.platform, installed.servicePath);
  const directory = mkdtempSync(join(tmpdir(), "roost-dryrun-existing-"));
  const rendered = join(directory, basename(installed.servicePath));
  writeFileSync(rendered, candidate, { mode: statSync(installed.servicePath).mode & 0o777 });
  console.log(`>> dry-run existing coordinator definition → ${rendered}`);
}
export async function runExistingQuickstart(options: {
  installed: ExistingQuickstartInstall;
  endpoint: QuickstartEndpoint;
  invocation: QuickstartOptions;
  provisionWorker: ExistingQuickstartProvisioner;
  readWorkerLifecycle?: typeof workerLifecycle;
}): Promise<ExistingQuickstartResult | null> {
  if (options.invocation.force) {
    throw new Error("--force is unavailable for an existing install; quickstart does not upgrade installed services");
  }
  const promotion = options.invocation.coordinatorUrl !== null;
  if (options.invocation.dryRun) {
    await dryRunExistingQuickstart(options.installed, options.endpoint, promotion);
    return null;
  }
  const installed = await _reactivateCoordinator(options.installed, options.endpoint, promotion);
  const endpoint = promotion
    ? resolveQuickstartEndpoint(["--coordinator-url", options.invocation.coordinatorUrl!], installed.platform, installed.environment)
    : resolveQuickstartEndpoint([], installed.platform, installed.environment);
  const databasePath = installed.environment.ROOST_COORDINATOR_DB!;
  const worker = await _discoverExistingWorker(installed.platform);
  let fingerprint: string;
  let lifecycle: WorkerLifecycle | null = null;
  if (!worker) {
    const bootstrapToken = await mintWorkerToken(databasePath, "quickstart-local-worker");
    await options.provisionWorker({ coordinatorUrl: quickstartLoopbackOrigin(endpoint), bootstrapToken });
    const registered = await waitForWorkerRegistration(databasePath, bootstrapToken);
    if (!registered) throw new Error("new local worker did not register with the coordinator");
    fingerprint = registered;
  } else {
    lifecycle = (options.readWorkerLifecycle ?? workerLifecycle)(worker, installed.platform);
    if (isRegisteredWorker(databasePath, worker.fingerprint)) {
      fingerprint = worker.fingerprint;
    } else {
      const bootstrapToken = await mintWorkerToken(databasePath, "quickstart-existing-worker");
      await enrollExistingWorker(worker, installed.platform, bootstrapToken);
      const registered = await waitForWorkerRegistration(databasePath, bootstrapToken);
      if (registered !== worker.fingerprint) {
        throw new Error("existing worker enrollment did not prove its installed key fingerprint");
      }
      fingerprint = registered;
    }
    if (lifecycle !== "running") {
      const started = await run([
        "bash",
        "-lc",
        startWorkerCmd(installed.platform, worker.servicePath, worker.serviceLabel),
      ], { quiet: true });
      if (started.exit !== 0) {
        throw new Error(`installed worker start failed (${started.exit}): ${started.stderr || started.stdout}`);
      }
    }
  }
  if (!await waitForWorkerRoutability(endpoint, fingerprint)) {
    throw new Error(`worker ${fingerprint.slice(0, 12)} did not become routable after coordinator activation`);
  }
  const remoteAccessVerified = promotion ? await remoteHealth(endpoint) : null;
  return { endpoint, databasePath, workerFingerprint: fingerprint, remoteAccessVerified };
}
