// Defines the exact Docker container contract for managed SaaS coordinators.
// The runtime uses these arguments and inspections before adopting a container.
// Strict equality prevents configuration drift from becoming tenant exposure.
import { isAbsolute } from "node:path";
import type { RegistryAccount, RegistryCoordinator } from "./registry.ts";
import type { InstanceLayout } from "./layout.ts";
import type { ManagedCredentialTopology } from "../../../coord/src/managed-container-invariant.ts";

const OUTPUT_LIMIT = 1024 * 1024;
export const HEALTH_TIMEOUT_MS = 120_000;
export const HEALTH_POLL_MS = 1_000;
export const TMPFS_SPEC = "rw,noexec,nosuid,size=67108864,uid=65532,gid=65532";
const SHARED_PUBLIC_ORIGIN = "https://dashboard.roosttt.com";
const ROUTE_KEY_RE = /^[0-9a-f]{64}$/;
export const SAAS_AUTH_VERIFY_KEY_CONTAINER_PATH = "/run/auth/saas-auth-verify-key";
export const SEED_STDIN_LIMIT = 1_024;
export const SHA256_HEX_RE = /^[0-9a-f]{64}$/;

export interface CommandResult {
  exitCode: number;
  stdout: string;
  stderr: string;
}

export type CommandRunner = (
  argv: readonly string[],
  stdin?: string,
) => Promise<CommandResult>;

export interface ManagedEmailRuntimeConfig {
  resendEndpoint: string;
  emailFrom: string;
  sharedResendApiKeyPath: string;
}

export interface ManagedInstanceSpec {
  account: RegistryAccount;
  coordinator: RegistryCoordinator;
  authVerifyKeyFile: string;
  email: ManagedEmailRuntimeConfig;
}

export interface ActivationStatus {
  activated: boolean;
  accountId: string;
  coordinatorId: string;
  expiresAtMs: number;
  topology: ManagedCredentialTopology;
}

export interface ManagedInstanceRuntimeOptions {
  runner?: CommandRunner;
  fetchImpl?: typeof fetch;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  network?: string;
  uid?: number;
  gid?: number;
  randomKey?: () => Uint8Array;
}

interface DockerInspectMount {
  Type?: string;
  Source?: string;
  Destination?: string;
  RW?: boolean;
}

export interface DockerInspect {
  Name?: string;
  Image?: string;
  Config?: {
    Image?: string;
    Env?: string[];
    Labels?: Record<string, string>;
    User?: string;
  };
  HostConfig?: {
    Privileged?: boolean;
    CapAdd?: string[];
    Devices?: unknown[];
    PidMode?: string;
    IpcMode?: string;
    UTSMode?: string;
    UsernsMode?: string;
    ReadonlyRootfs?: boolean;
    CapDrop?: string[];
    SecurityOpt?: string[];
    NanoCpus?: number;
    Memory?: number;
    PidsLimit?: number;
    NetworkMode?: string;
    Tmpfs?: Record<string, string>;
    PortBindings?: Record<string, unknown> | null;
    LogConfig?: { Type?: string; Config?: Record<string, string> };
  };
  Mounts?: DockerInspectMount[];
  NetworkSettings?: {
    Networks?: Record<string, { IPAddress?: string }>;
    Ports?: Record<string, unknown>;
  };
  State?: { Running?: boolean; Health?: { Status?: string } };
}

export async function defaultRunner(
  argv: readonly string[],
  stdin?: string,
): Promise<CommandResult> {
  if (argv.length === 0 || !argv[0]) throw new Error("empty command argv");
  if (stdin !== undefined && Buffer.byteLength(stdin, "utf8") > SEED_STDIN_LIMIT) {
    throw new Error("managed runtime stdin exceeded its bound");
  }
  const process = Bun.spawn([...argv], {
    stdout: "pipe",
    stderr: "pipe",
    stdin: stdin === undefined ? "ignore" : new Blob([stdin]),
  });
  let timedOut = false;
  const timeout = setTimeout(() => {
    timedOut = true;
    process.kill();
  }, 4 * 60_000);
  timeout.unref();
  const [exitCode, stdoutBytes, stderrBytes] = await Promise.all([
    process.exited,
    new Response(process.stdout).arrayBuffer(),
    new Response(process.stderr).arrayBuffer(),
  ]).finally(() => clearTimeout(timeout));
  if (timedOut) throw new Error("managed runtime command timed out");
  if (stdoutBytes.byteLength > OUTPUT_LIMIT || stderrBytes.byteLength > OUTPUT_LIMIT) {
    throw new Error("managed runtime command output exceeded its bound");
  }
  return {
    exitCode,
    stdout: new TextDecoder().decode(stdoutBytes),
    stderr: new TextDecoder().decode(stderrBytes),
  };
}

export function safeCommandFailure(action: string, result: CommandResult): never {
  throw new Error(`${action} failed with exit ${result.exitCode}`);
}

export function requiredEnvironment(spec: ManagedInstanceSpec, layout: InstanceLayout): Record<string, string> {
  if (!ROUTE_KEY_RE.test(spec.coordinator.routeKey)) {
    throw new Error("managed coordinator has an invalid tenant route key");
  }
  if (spec.account.routeKey !== spec.coordinator.routeKey) {
    throw new Error("managed account/coordinator route key mismatch");
  }
  if (!isAbsolute(spec.authVerifyKeyFile)) {
    throw new Error("managed SaaS auth verify key file must be absolute");
  }
  return {
    ROOST_COORDINATOR_INSTANCE_ID: spec.coordinator.id,
    ROOST_WEB_PUBLIC_URL: SHARED_PUBLIC_ORIGIN,
    ROOST_TENANT_ROUTE_KEY: spec.coordinator.routeKey,
    ROOST_COORDINATOR_DB: "/data/coordinator_v2.db",
    ROOST_COORDINATOR_AUTHORIZED_KEYS: "/data/authorized_keys.roost",
    ROOST_COORDINATOR_KEY_PATH: "/run/secrets/ssh_ed25519.key",
    ROOST_COORDINATOR_HANDOFF_PATH: "/data/coord-handoff.json",
    ROOST_COORDINATOR_LOG_DIR: "/data/logs",
    ROOST_RESEND_ENDPOINT: spec.email.resendEndpoint,
    ROOST_EMAIL_FROM: spec.email.emailFrom,
    ROOST_RESEND_API_KEY_FILE: "/run/secrets/resend-api-key",
    ROOST_EMAIL_OUTBOX_KEY_FILE: "/run/secrets/email-outbox-key",
    ROOST_SAAS_AUTH_VERIFY_KEY_FILE: SAAS_AUTH_VERIFY_KEY_CONTAINER_PATH,
    ROOST_MANAGED_CONTAINER: "1",
    ROOST_SAAS_MODE: "1",
    ROOST_TRUST_PROXY: "1",
    ROOST_COORDINATOR_BIND: "127.0.0.1:4103",
    ROOST_PUBLIC_BIND: "0.0.0.0:4104",
    ROOST_COORD_DATA_DIR: "/data",
    ROOST_COORD_LOG_DIR: "/data/logs",
  };
}

export function envArgv(environment: Record<string, string>): string[] {
  const argv: string[] = [];
  for (const [name, value] of Object.entries(environment).sort(([a], [b]) => a.localeCompare(b))) {
    if (/token|password/i.test(name)) throw new Error("refusing sensitive managed runtime environment name");
    argv.push("--env", `${name}=${value}`);
  }
  return argv;
}

export function expectedLabels(spec: ManagedInstanceSpec): Record<string, string> {
  return {
    "com.roost.saas": "coordinator",
    "com.roost.account-id": spec.account.id,
    "com.roost.coordinator-id": spec.coordinator.id,
    "com.roost.ordinal": String(spec.coordinator.ordinal),
  };
}

export function labelArgv(labels: Record<string, string>): string[] {
  const argv: string[] = [];
  for (const [name, value] of Object.entries(labels).sort(([a], [b]) => a.localeCompare(b))) {
    argv.push("--label", `${name}=${value}`);
  }
  return argv;
}

export function mountArg(source: string, destination: string, readonly: boolean): string {
  return `type=bind,src=${source},dst=${destination}${readonly ? ",readonly" : ""}`;
}

export function parseInspect(stdout: string): DockerInspect {
  if (Buffer.byteLength(stdout, "utf8") > OUTPUT_LIMIT) throw new Error("docker inspect output exceeded its bound");
  let parsed: unknown;
  try { parsed = JSON.parse(stdout); } catch { throw new Error("docker inspect returned invalid JSON"); }
  if (!Array.isArray(parsed) || parsed.length !== 1 || !parsed[0] || typeof parsed[0] !== "object") {
    throw new Error("docker inspect returned an unexpected shape");
  }
  return parsed[0] as DockerInspect;
}

function envMap(values: string[] | undefined): Record<string, string> {
  const mapped: Record<string, string> = {};
  for (const item of values ?? []) {
    const separator = item.indexOf("=");
    if (separator <= 0) throw new Error("container has malformed environment");
    mapped[item.slice(0, separator)] = item.slice(separator + 1);
  }
  return mapped;
}

export function assertExactContainer(
  inspect: DockerInspect,
  spec: ManagedInstanceSpec,
  layout: InstanceLayout,
  network: string,
): void {
  if (inspect.Name !== `/${spec.coordinator.containerName}`) throw new Error("container name mismatch");
  if (inspect.Image !== spec.coordinator.imageDigest || inspect.Config?.Image !== spec.coordinator.imageDigest) {
    throw new Error("container image digest mismatch");
  }
  if (inspect.Config?.User !== "65532:65532") throw new Error("container user mismatch");
  const labels = inspect.Config?.Labels ?? {};
  for (const [name, value] of Object.entries(expectedLabels(spec))) {
    if (labels[name] !== value) throw new Error(`container label mismatch: ${name}`);
  }
  const expectedEnv = requiredEnvironment(spec, layout);
  const actualEnv = envMap(inspect.Config?.Env);
  for (const [name, value] of Object.entries(expectedEnv)) {
    if (actualEnv[name] !== value) throw new Error(`container environment mismatch: ${name}`);
  }
  const roostExpected = new Set(Object.keys(expectedEnv));
  for (const name of Object.keys(actualEnv)) {
    if (name.startsWith("ROOST_") && !roostExpected.has(name)) {
      throw new Error(`container has unexpected Roost environment: ${name}`);
    }
  }
  const host = inspect.HostConfig;
  if (!host?.ReadonlyRootfs) throw new Error("container root filesystem is writable");
  if (host.NetworkMode !== network) throw new Error("container network mismatch");
  if (host.NanoCpus !== 1_000_000_000 || host.Memory !== 2 * 1024 * 1024 * 1024 || host.PidsLimit !== 256) {
    throw new Error("container resource limits mismatch");
  }
  if (host.Privileged
    || (host.CapAdd?.length ?? 0) !== 0
    || (host.Devices?.length ?? 0) !== 0
    || host.PidMode === "host"
    || host.IpcMode === "host"
    || host.UTSMode === "host"
    || host.UsernsMode === "host") {
    throw new Error("container has unexpected host privileges");
  }
  if (host.CapDrop?.length !== 1 || host.CapDrop[0] !== "ALL") {
    throw new Error("container capabilities mismatch");
  }
  if (host.SecurityOpt?.length !== 1 || host.SecurityOpt[0] !== "no-new-privileges") {
    throw new Error("container security options mismatch");
  }
  if (host.Tmpfs?.["/tmp"] !== TMPFS_SPEC) throw new Error("container tmpfs mismatch");
  if (host.LogConfig?.Type !== "json-file"
    || host.LogConfig.Config?.["max-size"] !== "10m"
    || host.LogConfig.Config?.["max-file"] !== "5") {
    throw new Error("container log limits mismatch");
  }
  if (host.PortBindings && Object.keys(host.PortBindings).length > 0) {
    throw new Error("container unexpectedly publishes ports");
  }
  const mounts = inspect.Mounts ?? [];
  if (mounts.length !== 3) throw new Error("container mount count mismatch");
  const dataMount = mounts.find((mount) => mount.Destination === "/data");
  const secretMount = mounts.find((mount) => mount.Destination === "/run/secrets");
  const authVerifyMount = mounts.find((mount) => mount.Destination === "/run/auth");
  if (dataMount?.Type !== "bind" || dataMount.Source !== layout.dataDir || dataMount.RW !== true) {
    throw new Error("container data mount mismatch");
  }
  if (secretMount?.Type !== "bind" || secretMount.Source !== layout.secretsDir || secretMount.RW !== false) {
    throw new Error("container secret mount mismatch");
  }
  if (authVerifyMount?.Type !== "bind"
    || authVerifyMount.Source !== layout.verifierDir
    || authVerifyMount.RW !== false) {
    throw new Error("container SaaS auth verifier mount mismatch");
  }
  const networks = inspect.NetworkSettings?.Networks ?? {};
  if (!networks[network] || Object.keys(networks).some((name) => name !== network)) {
    throw new Error("container network attachment mismatch");
  }
}
