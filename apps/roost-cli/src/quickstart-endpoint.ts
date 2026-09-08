// Endpoint selection is the no-effect boundary for `roost quickstart`. It
// validates the operator-declared front-door URL before any credential,
// service, filesystem, or build mutation can occur, and derives the loopback
// coordinator environment the installer persists from that single input.

/** Port the coordinator binds on loopback. The operator's front door proxies
 * to it; roost owns no TLS, DNS, or tunnel of its own. */
const QUICKSTART_LOOPBACK_PORT = 4103;

const COORDINATOR_URL_FLAG = "--coordinator-url";

export interface QuickstartEndpoint {
  /** Operator-declared front door, exactly as browsers and workers dial it. */
  origin: string;
  /** Loopback port the coordinator itself listens on, plaintext. */
  loopbackPort: number;
}

function quickstartFlagValue(args: readonly string[], name: string): string | null {
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

/**
 * Resolve the one endpoint input `roost quickstart` accepts. The URL must be an
 * absolute HTTPS origin with no credentials, path, query, or fragment; an
 * explicit port is optional and defaults to 443.
 */
export function resolveQuickstartEndpoint(
  args: readonly string[],
  env: Readonly<Record<string, string | undefined>>,
  platform: NodeJS.Platform,
): QuickstartEndpoint {
  // Ambient coordinator variables must never supply the front door: the
  // operator declares it per invocation. The parameter is injected to keep
  // that invariant observable without consulting process.env.
  void env;
  if (platform !== "darwin" && platform !== "linux" && platform !== "win32") {
    throw new Error(`unsupported quickstart platform: ${platform}`);
  }
  // Silently ignoring a retired flag would hand back a coordinator that
  // never reads the operator's certificate.
  for (const retired of ["--tls-cert", "--tls-key"]) {
    if (args.some((argument) => argument === retired || argument.startsWith(`${retired}=`))) {
      throw new Error(
        `${retired} is no longer accepted: the coordinator serves plaintext on loopback `
          + "and your front door owns TLS",
      );
    }
  }

  const rawUrl = quickstartFlagValue(args, COORDINATOR_URL_FLAG);
  if (rawUrl === null) {
    throw new Error(
      `${COORDINATOR_URL_FLAG} is required: the HTTPS URL your front door serves Roost on`,
    );
  }
  if (rawUrl.trim() !== rawUrl || /[\0\r\n\t]/.test(rawUrl)) {
    throw new Error(`${COORDINATOR_URL_FLAG} contains invalid whitespace`);
  }
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    throw new Error(`${COORDINATOR_URL_FLAG} must be a valid HTTPS URL`);
  }
  if (parsed.protocol !== "https:") {
    throw new Error(`${COORDINATOR_URL_FLAG} must use https`);
  }
  if (parsed.username || parsed.password) {
    throw new Error(`${COORDINATOR_URL_FLAG} must not contain username or password`);
  }
  if (parsed.search || parsed.hash) {
    throw new Error(`${COORDINATOR_URL_FLAG} must not contain a query or fragment`);
  }
  // URL parsing normalizes credentials and backslashes away, so the raw
  // authority is inspected before trusting the parsed origin.
  const authorityStart = rawUrl.indexOf("://") + 3;
  const separatorOffset = rawUrl.slice(authorityStart).search(/[/?#]/);
  const authorityEnd = separatorOffset === -1 ? rawUrl.length : authorityStart + separatorOffset;
  const authority = rawUrl.slice(authorityStart, authorityEnd);
  if (authority.includes("\\") || authority.includes("@")) {
    throw new Error(`${COORDINATOR_URL_FLAG} must not contain credentials or backslashes`);
  }
  const rawPath = rawUrl.slice(authorityEnd);
  if (rawPath !== "" && rawPath !== "/") {
    throw new Error(`${COORDINATOR_URL_FLAG} path must be exactly /`);
  }
  if (!parsed.hostname) throw new Error(`${COORDINATOR_URL_FLAG} must include a hostname`);
  // URL parsing accepts port 0 and drops the protocol default, so the explicit
  // port is range-checked here rather than inferred from the origin.
  if (parsed.port !== "" && Number(parsed.port) < 1) {
    throw new Error(`${COORDINATOR_URL_FLAG} port must be between 1 and 65535`);
  }

  return { origin: parsed.origin, loopbackPort: QUICKSTART_LOOPBACK_PORT };
}

/** The coordinator's own listener. Health polling and the local worker's first
 * dial use it because it is the only listener this install owns. */
export function quickstartLoopbackOrigin(endpoint: QuickstartEndpoint): string {
  return `http://127.0.0.1:${endpoint.loopbackPort}`;
}

/** Service-visible coordinator configuration: one loopback listener behind a
 * trusted proxy, told the public URL the operator put in front of it. */
export function coordinatorEnvironmentForQuickstart(
  endpoint: QuickstartEndpoint,
): Record<string, string> {
  return {
    ROOST_COORDINATOR_BIND: `127.0.0.1:${endpoint.loopbackPort}`,
    ROOST_TRUST_PROXY: "1",
    ROOST_WEB_PUBLIC_URL: endpoint.origin,
    ROOST_SKIP_ENV_LOCAL: "1",
  };
}
