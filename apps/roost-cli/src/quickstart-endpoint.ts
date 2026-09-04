// Endpoint selection is the no-effect boundary for `roost quickstart`.
// It validates explicit HTTPS and TLS inputs before any credential, service,
// filesystem, build, or Tailscale mutation can occur.

import * as nodeFs from "node:fs";
import { join, posix, win32 } from "node:path";

export type QuickstartEndpointMode = "automatic" | "explicit";

export interface QuickstartEndpoint {
  mode: QuickstartEndpointMode;
  origin: string | null;
  hostname: string | null;
  port: number | null;
  tlsCertPath: string | null;
  tlsKeyPath: string | null;
}

export type ResolvedQuickstartEndpoint = QuickstartEndpoint & {
  origin: string;
  hostname: string;
  port: number;
  tlsCertPath: string;
  tlsKeyPath: string;
};

export type QuickstartTlsFileSystem = Pick<
  typeof nodeFs,
  "accessSync" | "lstatSync" | "realpathSync" | "statSync" | "constants"
>;

const ENDPOINT_FLAGS = [
  "--coordinator-url",
  "--tls-cert",
  "--tls-key",
] as const;

function quickstartFlagValue(args: readonly string[], name: (typeof ENDPOINT_FLAGS)[number]): string | null {
  let value: string | null = null;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    const inline = argument.startsWith(`${name}=`) ? argument.slice(name.length + 1) : null;
    if (argument !== name && inline === null) continue;
    if (value !== null) throw new Error(`${name} may be provided only once`);
    const candidate = inline ?? args[index + 1];
    if (!candidate || (inline === null && candidate.startsWith("--"))) {
      throw new Error(`${name} requires a value`);
    }
    value = candidate;
    if (inline === null) index += 1;
  }
  return value;
}

function explicitUrlPort(authority: string): number {
  if (authority.includes("\\") || authority.includes("@")) {
    throw new Error("--coordinator-url must not contain credentials or backslashes");
  }
  const match = authority.startsWith("[")
    ? /^\[[^\]]+\]:(\d+)$/.exec(authority)
    : /^[^:]+:(\d+)$/.exec(authority);
  if (!match) throw new Error("--coordinator-url must include an explicit port");
  const port = Number(match[1]);
  if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) {
    throw new Error("--coordinator-url port must be between 1 and 65535");
  }
  return port;
}

/**
 * Resolve only the endpoint-selection inputs. With no endpoint flags this is
 * intentionally an automatic-mode sentinel: Tailscale discovery happens later
 * and only after all no-effect validation has completed.
 */
export function resolveQuickstartEndpoint(
  args: readonly string[],
  env: Readonly<Record<string, string | undefined>>,
  platform: NodeJS.Platform,
): QuickstartEndpoint {
  // Ambient coordinator/TLS variables must never turn a no-flag invocation
  // into explicit mode. The parameter is injected to keep this invariant
  // observable without consulting process.env.
  void env;
  if (platform !== "darwin" && platform !== "linux" && platform !== "win32") {
    throw new Error(`unsupported quickstart platform: ${platform}`);
  }

  const [coordinatorUrl, tlsCert, tlsKey] = ENDPOINT_FLAGS.map(
    (flag) => quickstartFlagValue(args, flag),
  );
  const supplied = [coordinatorUrl, tlsCert, tlsKey].filter((value) => value !== null).length;
  if (supplied === 0) {
    return {
      mode: "automatic",
      origin: null,
      hostname: null,
      port: null,
      tlsCertPath: null,
      tlsKeyPath: null,
    };
  }
  if (supplied !== ENDPOINT_FLAGS.length) {
    throw new Error("--coordinator-url, --tls-cert, and --tls-key must be provided together");
  }

  const rawUrl = coordinatorUrl!;
  if (rawUrl.trim() !== rawUrl || /[\0\r\n\t]/.test(rawUrl)) {
    throw new Error("--coordinator-url contains invalid whitespace");
  }
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    throw new Error("--coordinator-url must be a valid HTTPS URL");
  }
  if (parsed.protocol !== "https:") {
    throw new Error("--coordinator-url must use https");
  }
  if (parsed.username || parsed.password) {
    throw new Error("--coordinator-url must not contain username or password");
  }
  if (parsed.search || parsed.hash) {
    throw new Error("--coordinator-url must not contain a query or fragment");
  }
  const authorityStart = rawUrl.indexOf("://") + 3;
  const separatorOffset = rawUrl.slice(authorityStart).search(/[/?#]/);
  const authorityEnd = separatorOffset === -1 ? rawUrl.length : authorityStart + separatorOffset;
  const authority = rawUrl.slice(authorityStart, authorityEnd);
  const rawPath = rawUrl.slice(authorityEnd);
  if (rawPath !== "" && rawPath !== "/") {
    throw new Error("--coordinator-url path must be exactly /");
  }
  const port = explicitUrlPort(authority);
  if (!parsed.hostname) throw new Error("--coordinator-url must include a hostname");

  const pathApi = platform === "win32" ? win32 : posix;
  for (const [flag, value] of [["--tls-cert", tlsCert!], ["--tls-key", tlsKey!]] as const) {
    if (/[\0\r\n]/.test(value) || !pathApi.isAbsolute(value)) {
      throw new Error(`${flag} must be an absolute path`);
    }
  }
  const certPath = pathApi.normalize(tlsCert!);
  const keyPath = pathApi.normalize(tlsKey!);
  const certIdentity = platform === "win32" ? certPath.toLocaleLowerCase("en-US") : certPath;
  const keyIdentity = platform === "win32" ? keyPath.toLocaleLowerCase("en-US") : keyPath;
  if (certIdentity === keyIdentity) {
    throw new Error("--tls-cert and --tls-key must be distinct paths");
  }

  return {
    mode: "explicit",
    origin: parsed.origin,
    hostname: parsed.hostname,
    port,
    tlsCertPath: certPath,
    tlsKeyPath: keyPath,
  };
}

/**
 * Validate direct TLS inputs without mutating them. Both lexical aliases and
 * filesystem aliases (hard links, parent-directory links, or other paths to
 * the same underlying file) are rejected.
 */
export function validateQuickstartTlsFiles(
  endpoint: QuickstartEndpoint,
  fs: QuickstartTlsFileSystem = nodeFs,
): void {
  if (endpoint.mode === "automatic") return;
  const inspect = (path: string, label: string) => {
    let initial: nodeFs.Stats;
    try {
      initial = fs.lstatSync(path);
    } catch {
      throw new Error(`${label} is not accessible`);
    }
    if (initial.isSymbolicLink() || !initial.isFile()) {
      throw new Error(`${label} must be a non-symlink regular file`);
    }
    try {
      fs.accessSync(path, fs.constants.R_OK);
    } catch {
      throw new Error(`${label} is not readable`);
    }
    let resolvedPath: string;
    let resolved: nodeFs.Stats;
    try {
      resolvedPath = fs.realpathSync(path);
      resolved = fs.statSync(resolvedPath);
    } catch {
      throw new Error(`${label} could not be resolved`);
    }
    if (!resolved.isFile()) throw new Error(`${label} must resolve to a regular file`);
    return { resolvedPath, dev: resolved.dev, ino: resolved.ino };
  };
  const cert = inspect(endpoint.tlsCertPath!, "--tls-cert");
  const key = inspect(endpoint.tlsKeyPath!, "--tls-key");
  if (cert.resolvedPath === key.resolvedPath || (cert.dev === key.dev && cert.ino === key.ino)) {
    throw new Error("--tls-cert and --tls-key must identify distinct files");
  }
}

export function automaticQuickstartEndpoint(fqdn: string, tlsDir: string): ResolvedQuickstartEndpoint {
  const origin = new URL(`https://${fqdn}:4102`).origin;
  return {
    mode: "automatic",
    origin,
    hostname: fqdn,
    port: 4102,
    tlsCertPath: join(tlsDir, `${fqdn}.crt`),
    tlsKeyPath: join(tlsDir, `${fqdn}.key`),
  };
}

export function requireResolvedEndpoint(
  endpoint: QuickstartEndpoint,
): asserts endpoint is ResolvedQuickstartEndpoint {
  if (
    !endpoint.origin
    || !endpoint.hostname
    || endpoint.port === null
    || !endpoint.tlsCertPath
    || !endpoint.tlsKeyPath
  ) {
    throw new Error("quickstart endpoint has not been resolved");
  }
}

/**
 * Service-visible endpoint configuration. Automatic POSIX installs retain the
 * existing Tailscale Serve topology. Automatic Windows installs retain their
 * direct tailnet certificate listener, while the tailnet-port marker records
 * that Tailscale is still required. Explicit mode is direct on every platform.
 */
export function coordinatorEnvironmentForQuickstart(
  endpoint: QuickstartEndpoint,
  platform: NodeJS.Platform,
): Record<string, string> {
  requireResolvedEndpoint(endpoint);
  if (endpoint.mode === "automatic" && platform !== "win32") {
    return {
      ROOST_FRONTED: "1",
      ROOST_COORD_LOOPBACK_PORT: "4103",
      ROOST_TAILNET_HTTPS_PORT: String(endpoint.port),
      ROOST_COORDINATOR_PUBLIC_URL: endpoint.origin,
    };
  }
  return {
    ROOST_FRONTED: "0",
    ROOST_COORDINATOR_BIND: `0.0.0.0:${endpoint.port}`,
    ROOST_COORDINATOR_PUBLIC_URL: endpoint.origin,
    ROOST_TLS_CERT_PATH: endpoint.tlsCertPath,
    ROOST_TLS_KEY_PATH: endpoint.tlsKeyPath,
    ...(endpoint.mode === "explicit" && platform !== "win32" ? { ROOST_SKIP_ENV_LOCAL: "1" } : {}),
    ...(endpoint.mode === "automatic"
      ? { ROOST_TAILNET_HTTPS_PORT: String(endpoint.port) }
      : {}),
  };
}
