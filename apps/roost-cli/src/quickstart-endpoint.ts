// Endpoint selection is the no-effect boundary for `roost quickstart`. It
// validates invocation syntax and declared origins before service, credential,
// build, or filesystem mutation, then derives the profile installers persist.
// Existing-service discovery supplies its validated environment to retain an
// installed front door instead of consulting the invoking shell.

const QUICKSTART_LOOPBACK_PORT = 4103;
const COORDINATOR_URL_FLAG = "--coordinator-url";
const LOCAL_HOST = "127.0.0.1";

export interface QuickstartEndpoint {
  mode: "local" | "front-door";
  /** Browser destination for this invocation. */
  origin: string;
  /** Actual coordinator listener port, always on canonical loopback. */
  loopbackPort: number;
  /** Browser front door persisted by the coordinator, when configured. */
  webPublicUrl: string | null;
  /** Separately declared worker dial door, never inferred from the browser URL. */
  coordinatorPublicUrl: string | null;
  /** Exact browser origins the coordinator admits through CORS. */
  corsAllowedOrigins: readonly string[];
}

export interface QuickstartOptions {
  coordinatorUrl: string | null;
  dryRun: boolean;
  force: boolean;
  windowsServiceCredentialStdin: boolean;
}

function canonicalLoopbackOrigin(port: number): string {
  return `http://${LOCAL_HOST}:${port}`;
}

function exactFlagValue(
  args: readonly string[],
  name: string,
): string | null {
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

function exactBooleanFlag(args: readonly string[], name: string): boolean {
  let present = false;
  for (const argument of args) {
    if (argument === name) {
      if (present) throw new Error(`${name} may be provided only once`);
      present = true;
      continue;
    }
    if (argument.startsWith(`${name}=`)) {
      throw new Error(`${name} does not accept a value`);
    }
  }
  return present;
}

/** Parse the entire quickstart argv so a misspelled flag cannot become a
 * positional no-op after the no-effect boundary. */
export function parseQuickstartOptions(args: readonly string[]): QuickstartOptions {
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index]!;
    if (argument === COORDINATOR_URL_FLAG) {
      if (index + 1 >= args.length || args[index + 1]!.startsWith("--")) {
        throw new Error(`${COORDINATOR_URL_FLAG} requires a value`);
      }
      index += 1;
      continue;
    }
    if (argument.startsWith(`${COORDINATOR_URL_FLAG}=`)) continue;
    if (argument === "--dry-run" || argument === "--force"
      || argument === "--windows-service-credential-stdin") {
      continue;
    }
    const retiredFlag = argument === "--tls-cert" || argument.startsWith("--tls-cert=")
      ? "--tls-cert"
      : argument === "--tls-key" || argument.startsWith("--tls-key=")
        ? "--tls-key"
        : null;
    if (retiredFlag) {
      throw new Error(
        `${retiredFlag} is no longer accepted: the coordinator serves plaintext on loopback `
          + "and your front door owns TLS",
      );
    }
    if (argument.startsWith("--")) throw new Error(`unknown quickstart option: ${argument}`);
    throw new Error(`unexpected quickstart argument: ${argument}`);
  }
  return {
    coordinatorUrl: exactFlagValue(args, COORDINATOR_URL_FLAG),
    dryRun: exactBooleanFlag(args, "--dry-run"),
    force: exactBooleanFlag(args, "--force"),
    windowsServiceCredentialStdin: exactBooleanFlag(args, "--windows-service-credential-stdin"),
  };
}

function parseHttpsOrigin(rawUrl: string, envName: string): string {
  if (rawUrl.trim() !== rawUrl || /[\0\r\n\t]/.test(rawUrl)) {
    throw new Error(`${envName} contains invalid whitespace`);
  }
  const authorityStart = rawUrl.indexOf("://") + 3;
  const separatorOffset = rawUrl.slice(authorityStart).search(/[/?#]/);
  const authorityEnd = separatorOffset === -1 ? rawUrl.length : authorityStart + separatorOffset;
  const authority = rawUrl.slice(authorityStart, authorityEnd);
  if (authority.includes("\\") || authority.includes("@")) {
    throw new Error(`${envName} must not contain credentials or backslashes`);
  }
  let parsed: URL;
  try {
    parsed = new URL(rawUrl);
  } catch {
    throw new Error(`${envName} must be a valid HTTPS URL`);
  }
  if (parsed.protocol !== "https:") throw new Error(`${envName} must use https`);
  if (parsed.username || parsed.password) {
    throw new Error(`${envName} must not contain username or password`);
  }
  if (parsed.search || parsed.hash) {
    throw new Error(`${envName} must not contain a query or fragment`);
  }
  const rawPath = rawUrl.slice(authorityEnd);
  if (rawPath !== "" && rawPath !== "/") {
    throw new Error(`${envName} path must be exactly /`);
  }
  if (!parsed.hostname) throw new Error(`${envName} must include a hostname`);
  if (parsed.port !== "" && (!/^[1-9]\d{0,4}$/.test(parsed.port) || Number(parsed.port) > 65535)) {
    throw new Error(`${envName} port must be between 1 and 65535`);
  }
  return parsed.origin;
}

function configuredHttpsOrigin(
  environment: Readonly<Record<string, string | undefined>>,
  name: "ROOST_WEB_PUBLIC_URL" | "ROOST_COORDINATOR_PUBLIC_URL",
): string | null {
  const value = environment[name];
  return value === undefined || value === "" ? null : parseHttpsOrigin(value, name);
}

function configuredLoopbackPort(
  environment: Readonly<Record<string, string | undefined>>,
): number {
  const bind = environment.ROOST_COORDINATOR_BIND;
  const match = bind && /^127\.0\.0\.1:([1-9]\d{0,4})$/.exec(bind);
  const port = match ? Number(match[1]) : Number.NaN;
  if (!Number.isSafeInteger(port) || port > 65535) {
    throw new Error("installed ROOST_COORDINATOR_BIND must be 127.0.0.1:<port>");
  }
  return port;
}

function configuredCorsOrigins(
  environment: Readonly<Record<string, string | undefined>>,
  canonicalLocalOrigin: string,
  mergeLocal: boolean,
): readonly string[] {
  const raw = environment.ROOST_CORS_ALLOWED_ORIGINS ?? "";
  const origins = raw === ""
    ? []
    : raw.split(",").map((value) => value.trim()).filter(Boolean);
  for (const origin of origins) {
    let parsed: URL;
    try {
      parsed = new URL(origin);
    } catch {
      throw new Error(`ROOST_CORS_ALLOWED_ORIGINS contains an invalid origin: ${origin}`);
    }
    if (!["http:", "https:"].includes(parsed.protocol) || parsed.origin !== origin) {
      throw new Error(`ROOST_CORS_ALLOWED_ORIGINS entries must be bare HTTP(S) origins: ${origin}`);
    }
  }
  if (mergeLocal && !origins.includes(canonicalLocalOrigin)) origins.push(canonicalLocalOrigin);
  return Object.freeze(origins);
}

function endpoint(
  mode: QuickstartEndpoint["mode"],
  origin: string,
  loopbackPort: number,
  webPublicUrl: string | null,
  coordinatorPublicUrl: string | null,
  corsAllowedOrigins: readonly string[],
): QuickstartEndpoint {
  return Object.freeze({
    mode,
    origin,
    loopbackPort,
    webPublicUrl,
    coordinatorPublicUrl,
    corsAllowedOrigins,
  });
}

/**
 * Resolve fresh, rerun, and promotion endpoint state without consulting
 * ambient endpoint variables. A non-null installed environment is authoritative
 * only after its owning service definition has been parsed and validated.
 */
export function resolveQuickstartEndpoint(
  args: readonly string[],
  platform: NodeJS.Platform,
  installedEnvironment: Readonly<Record<string, string | undefined>> | null = null,
): QuickstartEndpoint {
  if (platform !== "darwin" && platform !== "linux" && platform !== "win32") {
    throw new Error(`unsupported quickstart platform: ${platform}`);
  }
  const options = parseQuickstartOptions(args);
  const declaredOrigin = options.coordinatorUrl === null
    ? null
    : parseHttpsOrigin(options.coordinatorUrl, COORDINATOR_URL_FLAG);
  if (!installedEnvironment) {
    if (!declaredOrigin) {
      if (platform === "win32") {
        throw new Error("Windows quickstart requires --coordinator-url https://your.host");
      }
      const origin = canonicalLoopbackOrigin(QUICKSTART_LOOPBACK_PORT);
      return endpoint("local", origin, QUICKSTART_LOOPBACK_PORT, null, null, Object.freeze([origin]));
    }
    const localOrigin = canonicalLoopbackOrigin(QUICKSTART_LOOPBACK_PORT);
    return endpoint("front-door", declaredOrigin, QUICKSTART_LOOPBACK_PORT, declaredOrigin, null, Object.freeze([localOrigin]));
  }

  const loopbackPort = configuredLoopbackPort(installedEnvironment);
  const localOrigin = canonicalLoopbackOrigin(loopbackPort);
  const installedWebPublicUrl = configuredHttpsOrigin(installedEnvironment, "ROOST_WEB_PUBLIC_URL");
  const coordinatorPublicUrl = configuredHttpsOrigin(installedEnvironment, "ROOST_COORDINATOR_PUBLIC_URL");
  if (declaredOrigin) {
    return endpoint(
      "front-door",
      declaredOrigin,
      loopbackPort,
      declaredOrigin,
      coordinatorPublicUrl,
      configuredCorsOrigins(installedEnvironment, localOrigin, true),
    );
  }
  return endpoint(
    installedWebPublicUrl ? "front-door" : "local",
    installedWebPublicUrl ?? localOrigin,
    loopbackPort,
    installedWebPublicUrl,
    coordinatorPublicUrl,
    configuredCorsOrigins(installedEnvironment, localOrigin, false),
  );
}

/** The coordinator's listener and the first local worker both use this exact
 * canonical origin. It never follows an external browser/front-door origin. */
export function quickstartLoopbackOrigin(endpoint: QuickstartEndpoint): string {
  return canonicalLoopbackOrigin(endpoint.loopbackPort);
}

/** Fresh installation profile. Existing-install transitions edit only their
 * dedicated endpoint fields and otherwise reuse the installed definition. */
export function coordinatorEnvironmentForQuickstart(
  endpoint: QuickstartEndpoint,
): Record<string, string> {
  return {
    ROOST_COORDINATOR_BIND: `${LOCAL_HOST}:${endpoint.loopbackPort}`,
    ROOST_TRUST_PROXY: endpoint.mode === "front-door" ? "1" : "0",
    ROOST_WEB_PUBLIC_URL: endpoint.webPublicUrl ?? "",
    ROOST_COORDINATOR_PUBLIC_URL: endpoint.coordinatorPublicUrl ?? "",
    ROOST_CORS_ALLOWED_ORIGINS: endpoint.corsAllowedOrigins.join(","),
    ROOST_SKIP_ENV_LOCAL: "1",
  };
}
