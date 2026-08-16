import { createHash, randomBytes, timingSafeEqual } from "node:crypto";
import {
  chmodSync,
  closeSync,
  mkdirSync,
  openSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, win32 } from "node:path";
import { assertNeverPlatform, supportedHostPlatform, type SupportedHostPlatform } from "./platform.ts";
import { runWindowsHelper, runWindowsHelperSync, type RunWindowsHelperOptions } from "./windows-helper.ts";

export const LOCAL_ENDPOINT_UNAUTHENTICATED_MAX_BYTES = 64 * 1024;
export const LOCAL_ENDPOINT_UNAUTHENTICATED_TIMEOUT_MS = 2_000;
export const LOCAL_ENDPOINT_MAX_UNAUTHENTICATED_CONNECTIONS = 16;

export interface LocalEndpoint {
  platform: SupportedHostPlatform;
  kind: "uds" | "named-pipe";
  address: string;
  capability: string;
  capabilityPath: string;
  isFilesystemPath: boolean;
}

export interface ResolveLocalEndpointOptions {
  name: string;
  dataDir: string;
  platform?: SupportedHostPlatform;
  env?: Record<string, string | undefined>;
  helper?: RunWindowsHelperOptions;
}

function validateEndpointName(name: string): string {
  if (!/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(name)) throw new Error(`invalid local endpoint name: ${name}`);
  return name;
}

function loadOrCreateCapability(path: string, platform: SupportedHostPlatform, helper?: RunWindowsHelperOptions): string {
  const parent = platform === "win32" ? win32.dirname(path) : dirname(path);
  mkdirSync(parent, { recursive: true, mode: 0o700 });
  try {
    const capability = readFileSync(path, "utf8").trim();
    if (!/^[a-f0-9]{64}$/.test(capability)) throw new Error(`invalid local endpoint capability: ${path}`);
    return capability;
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
  }
  const capability = randomBytes(32).toString("hex");
  let fd: number;
  try {
    fd = openSync(path, "wx", 0o600);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "EEXIST") {
      const winner = readFileSync(path, "utf8").trim();
      if (!/^[a-f0-9]{64}$/.test(winner)) throw new Error(`invalid local endpoint capability: ${path}`);
      return winner;
    }
    throw error;
  }
  try {
    writeFileSync(fd, `${capability}\n`, "utf8");
  } finally {
    closeSync(fd);
  }
  if (platform === "win32") {
    runWindowsHelperSync<{ ok: true }>("apply-dacl", [path], helper);
  } else {
    chmodSync(path, 0o600);
  }
  return capability;
}

function currentWindowsSid(env: Record<string, string | undefined>, helper?: RunWindowsHelperOptions): string {
  if (env.ROOST_WINDOWS_SID) return env.ROOST_WINDOWS_SID;
  const result = runWindowsHelperSync<{ sid: string }>("current-user-sid", [], helper);
  if (!/^S-1-(?:\d+-)+\d+$/.test(result.sid)) throw new Error("roost-win-helper returned an invalid user SID");
  return result.sid;
}

/** Resolve the same stable endpoint in worker and keeper processes. */
export function resolveLocalEndpoint(options: ResolveLocalEndpointOptions): LocalEndpoint {
  const platform = options.platform ?? supportedHostPlatform();
  const env = options.env ?? process.env;
  const name = validateEndpointName(options.name);
  const capabilityPath = platform === "win32"
    ? win32.join(options.dataDir, `${name}.cap`)
    : join(options.dataDir, `${name}.cap`);
  const capability = loadOrCreateCapability(capabilityPath, platform, options.helper);
  switch (platform) {
    case "darwin":
    case "linux":
      return {
        platform,
        kind: "uds",
        address: join(options.dataDir, `${name}.sock`),
        capability,
        capabilityPath,
        isFilesystemPath: true,
      };
    case "win32": {
      const sid = currentWindowsSid(env, options.helper);
      const nonce = createHash("sha256").update(`${sid}\0${name}\0${capability}`).digest("hex").slice(0, 32);
      return {
        platform,
        kind: "named-pipe",
        address: `\\\\.\\pipe\\roost-${name}-${nonce}`,
        capability,
        capabilityPath,
        isFilesystemPath: false,
      };
    }
    default:
      return assertNeverPlatform(platform);
  }
}

export function localEndpointEnv(
  endpoint: LocalEndpoint,
  prefix = "ROOST_LOCAL",
): Record<string, string> {
  if (!/^[A-Z][A-Z0-9_]*$/.test(prefix)) throw new Error(`invalid local endpoint environment prefix: ${prefix}`);
  return {
    [`${prefix}_ENDPOINT`]: endpoint.address,
    [`${prefix}_CAPABILITY`]: endpoint.capability,
    [`${prefix}_ENDPOINT_KIND`]: endpoint.kind,
    [`${prefix}_CAPABILITY_PATH`]: endpoint.capabilityPath,
  };
}

export function localEndpointFromEnv(
  env: Record<string, string | undefined> = process.env,
  prefix = "ROOST_LOCAL",
  platform: SupportedHostPlatform = supportedHostPlatform(),
): LocalEndpoint {
  const address = env[`${prefix}_ENDPOINT`];
  const capability = env[`${prefix}_CAPABILITY`];
  const kind = env[`${prefix}_ENDPOINT_KIND`];
  if (!address || !capability || (kind !== "uds" && kind !== "named-pipe")) {
    throw new Error(`missing or invalid ${prefix} local endpoint environment`);
  }
  if (!/^[a-f0-9]{64}$/.test(capability)) throw new Error(`invalid ${prefix} capability`);
  const expectedKind = platform === "win32" ? "named-pipe" : "uds";
  if (kind !== expectedKind) throw new Error(`${prefix} endpoint kind does not match ${platform}`);
  if (kind === "named-pipe" && !address.startsWith("\\\\.\\pipe\\")) throw new Error(`invalid ${prefix} named pipe`);
  if (kind === "uds" && !address.startsWith("/")) throw new Error(`invalid ${prefix} Unix socket`);
  return {
    platform,
    kind,
    address,
    capability,
    capabilityPath: env[`${prefix}_CAPABILITY_PATH`] ?? "",
    isFilesystemPath: kind === "uds",
  };
}

export async function prepareLocalEndpoint(endpoint: LocalEndpoint): Promise<void> {
  if (!endpoint.isFilesystemPath) return;
  mkdirSync(dirname(endpoint.address), { recursive: true, mode: 0o700 });
  rmSync(endpoint.address, { force: true });
}

export async function secureLocalEndpoint(
  endpoint: LocalEndpoint,
  helper: RunWindowsHelperOptions = {},
): Promise<void> {
  if (endpoint.kind === "uds") {
    chmodSync(endpoint.address, 0o600);
    return;
  }
  if (!endpoint.capabilityPath) throw new Error("named-pipe endpoint is missing its capability path");
  await runWindowsHelper<{ ok: true }>("apply-dacl", [endpoint.capabilityPath], helper);
}

export async function cleanupLocalEndpoint(endpoint: LocalEndpoint): Promise<void> {
  if (endpoint.isFilesystemPath) rmSync(endpoint.address, { force: true });
}

export function verifyLocalEndpointCapability(expected: string, received: unknown): boolean {
  if (typeof received !== "string") return false;
  const expectedDigest = createHash("sha256").update(expected).digest();
  const receivedDigest = createHash("sha256").update(received).digest();
  return timingSafeEqual(expectedDigest, receivedDigest);
}
