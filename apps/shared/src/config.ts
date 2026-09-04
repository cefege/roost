// Loads coordinator configuration from environment variables during boot.
// It normalizes external input, resolves secret files, and enforces cross-field policy.
// The declarative configuration shape lives separately in coord-config-schema.ts.

import { closeSync, fstatSync, openSync, readSync } from "node:fs";
import { isAbsolute, join } from "node:path";
import { CoordConfig } from "./coord-config-schema.ts";
import { coordDataDir } from "./paths.ts";
import { resolveTailnetDnsName } from "./tailnet.ts";
import { isTenantRouteKey } from "./tenant-route.ts";

export { CoordConfig };

function normalizeHttpsOrigin(raw: string | undefined, envName: string): string | undefined {
  if (!raw) return undefined;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`${envName} must be a valid HTTPS origin`);
  }
  if (
    url.protocol !== "https:" || url.username || url.password || url.search ||
    url.hash || url.pathname !== "/"
  ) {
    throw new Error(`${envName} must be an HTTPS origin without credentials, query, fragment, or path`);
  }
  return url.origin;
}

function normalizeHttpsEndpoint(raw: string | undefined, envName: string): string | undefined {
  if (!raw) return undefined;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new Error(`${envName} must be a valid HTTPS URL`);
  }
  if (url.protocol !== "https:" || url.username || url.password || url.search || url.hash) {
    throw new Error(`${envName} must be an HTTPS URL without credentials, query, or fragment`);
  }
  return url.toString();
}

function require32ByteBase64Key(raw: string, envName: string): void {
  let encoding: "base64" | "base64url";
  if (/^[A-Za-z0-9+/]{43}=?$/.test(raw)) {
    encoding = "base64";
  } else if (/^[A-Za-z0-9_-]{43}=?$/.test(raw)) {
    encoding = "base64url";
  } else {
    throw new Error(`${envName} must be a canonical 32-byte base64 or base64url value`);
  }

  const bytes = Buffer.from(raw, encoding);
  const canonical = encoding === "base64"
    ? bytes.toString("base64").slice(0, -1)
    : bytes.toString("base64url");
  if (bytes.byteLength !== 32 || canonical !== raw.replace(/=$/, "")) {
    throw new Error(`${envName} must be a canonical 32-byte base64 or base64url value`);
  }
}

const SECRET_FILE_MAX_BYTES = 64 * 1024;
const CANONICAL_UUID_RE =
  /^[0-9a-f]{8}-[0-9a-f]{4}-[1-8][0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;

export const MANAGED_WEB_PUBLIC_ORIGIN = "https://dashboard.roosttt.com";

function readSecretFile(path: string, envName: string): string {
  if (!path.trim()) throw new Error(`${envName} must name a readable regular file`);

  let fd: number;
  try {
    fd = openSync(path, "r");
  } catch {
    throw new Error(`${envName} must name a readable regular file`);
  }

  try {
    const stat = fstatSync(fd);
    if (!stat.isFile()) {
      throw new Error(`${envName} must name a readable regular file`);
    }
    if (stat.size > SECRET_FILE_MAX_BYTES) {
      throw new Error(`${envName} exceeds the ${SECRET_FILE_MAX_BYTES}-byte limit`);
    }

    // Read at most one byte over the limit. Checking fstat alone would leave a
    // growth race that could allocate an attacker-sized file during startup.
    const bytes = Buffer.allocUnsafe(SECRET_FILE_MAX_BYTES + 1);
    let length = 0;
    while (length < bytes.byteLength) {
      const read = readSync(fd, bytes, length, bytes.byteLength - length, null);
      if (read === 0) break;
      length += read;
    }
    if (length > SECRET_FILE_MAX_BYTES) {
      throw new Error(`${envName} exceeds the ${SECRET_FILE_MAX_BYTES}-byte limit`);
    }

    let value: string;
    try {
      value = new TextDecoder("utf-8", { fatal: true })
        .decode(bytes.subarray(0, length))
        .trim();
    } catch {
      throw new Error(`${envName} must contain valid UTF-8`);
    }
    if (!value) throw new Error(`${envName} must not be empty`);
    return value;
  } finally {
    closeSync(fd);
  }
}

function loadSecret(
  env: Record<string, string | undefined>,
  directName: string,
  fileName: string,
): { provided: boolean; value: string | undefined } {
  const directProvided = env[directName] !== undefined;
  const fileProvided = env[fileName] !== undefined;
  if (directProvided && fileProvided) {
    throw new Error(`${directName} and ${fileName} cannot both be configured`);
  }
  if (fileProvided) {
    return { provided: true, value: readSecretFile(env[fileName]!, fileName) };
  }
  return { provided: directProvided, value: env[directName] };
}

export function loadCoordConfig(env: Record<string, string | undefined> = process.env): CoordConfig {
  const dataDir = coordDataDir(env);
  const managedContainer = env.ROOST_MANAGED_CONTAINER === "1";
  const tailnetDnsName = managedContainer ? "" : resolveTailnetDnsName();
  const resendApiKey = loadSecret(
    env,
    "ROOST_RESEND_API_KEY",
    "ROOST_RESEND_API_KEY_FILE",
  );
  const emailOutboxKey = loadSecret(
    env,
    "ROOST_EMAIL_OUTBOX_KEY",
    "ROOST_EMAIL_OUTBOX_KEY_FILE",
  );
  const parsed = CoordConfig.parse({
    bind: env.ROOST_COORDINATOR_BIND,
    dbPath: env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db"),
    authorizedKeysPath: env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(dataDir, "authorized_keys.roost"),
    webDistPath: env.ROOST_WEB_DIST_PATH,
    coordKeyPath: env.ROOST_COORDINATOR_KEY_PATH ?? join(dataDir, "ssh_ed25519.key"),
    jwtMaxAgeSecs: env.ROOST_COORDINATOR_JWT_MAX_AGE_SECS
      ? Number(env.ROOST_COORDINATOR_JWT_MAX_AGE_SECS)
      : undefined,
    auditRetentionDays: env.ROOST_COORDINATOR_AUDIT_RETENTION_DAYS
      ? Number(env.ROOST_COORDINATOR_AUDIT_RETENTION_DAYS)
      : undefined,
    corsAllowedOrigins: env.ROOST_CORS_ALLOWED_ORIGINS
      ? env.ROOST_CORS_ALLOWED_ORIGINS.split(",").map((s) => s.trim()).filter(Boolean)
      : [],
    pushAllowedOrigins: env.ROOST_PUSH_ALLOWED_ORIGINS
      ? env.ROOST_PUSH_ALLOWED_ORIGINS.split(",").map((s) => s.trim()).filter(Boolean)
      : [],
    relaxedCsp: env.ROOST_RELAXED_CSP === "1",
    trustProxy: env.ROOST_TRUST_PROXY === "1",
    publicBind: env.ROOST_PUBLIC_BIND,
    webPublicUrl: normalizeHttpsOrigin(env.ROOST_WEB_PUBLIC_URL, "ROOST_WEB_PUBLIC_URL"),
    saasMode: env.ROOST_SAAS_MODE === "1",
    managedContainer,
    instanceId: env.ROOST_COORDINATOR_INSTANCE_ID,
    tenantRouteKey: env.ROOST_TENANT_ROUTE_KEY,
    saasAuthVerifyKeyPath: env.ROOST_SAAS_AUTH_VERIFY_KEY_FILE,
    resendEndpoint: normalizeHttpsEndpoint(env.ROOST_RESEND_ENDPOINT, "ROOST_RESEND_ENDPOINT"),
    resendApiKey: resendApiKey.value,
    emailFrom: env.ROOST_EMAIL_FROM,
    emailOutboxKey: emailOutboxKey.value,
    cfAccessTeamDomain: env.ROOST_CF_ACCESS_TEAM_DOMAIN,
    cfAccessAud: env.ROOST_CF_ACCESS_AUD,
    logDir: env.ROOST_COORDINATOR_LOG_DIR,
    tlsCertPath: env.ROOST_TLS_CERT_PATH,
    tlsKeyPath: env.ROOST_TLS_KEY_PATH,
    publicUrl: normalizeHttpsOrigin(
      env.ROOST_COORDINATOR_PUBLIC_URL
        ?? (tailnetDnsName ? `https://${tailnetDnsName}:4102` : undefined),
      "ROOST_COORDINATOR_PUBLIC_URL",
    ),
    handoffPath: env.ROOST_COORDINATOR_HANDOFF_PATH ?? join(dataDir, "coord-handoff.json"),
  });

  if (parsed.saasMode && !parsed.webPublicUrl?.trim()) {
    throw new Error("ROOST_SAAS_MODE=1 requires ROOST_WEB_PUBLIC_URL");
  }

  const emailSettings = [
    parsed.resendEndpoint,
    parsed.resendApiKey,
    parsed.emailFrom,
    parsed.emailOutboxKey,
  ];
  const providedEmailSettingCount = [
    env.ROOST_RESEND_ENDPOINT !== undefined,
    resendApiKey.provided,
    env.ROOST_EMAIL_FROM !== undefined,
    emailOutboxKey.provided,
  ].filter(Boolean).length;
  if (providedEmailSettingCount !== 0 && providedEmailSettingCount !== emailSettings.length) {
    throw new Error(
      "ROOST_RESEND_ENDPOINT, ROOST_RESEND_API_KEY(_FILE), ROOST_EMAIL_FROM, and ROOST_EMAIL_OUTBOX_KEY(_FILE) must all be configured together",
    );
  }
  if (providedEmailSettingCount === emailSettings.length) {
    if (emailSettings.some((value) => !value?.trim())) {
      throw new Error(
        "ROOST_RESEND_ENDPOINT, ROOST_RESEND_API_KEY(_FILE), ROOST_EMAIL_FROM, and ROOST_EMAIL_OUTBOX_KEY(_FILE) must all be non-empty",
      );
    }
    require32ByteBase64Key(parsed.emailOutboxKey!, "ROOST_EMAIL_OUTBOX_KEY");
  }

  if (!parsed.managedContainer && env.ROOST_COORDINATOR_INSTANCE_ID !== undefined) {
    throw new Error("ROOST_COORDINATOR_INSTANCE_ID requires ROOST_MANAGED_CONTAINER=1");
  }
  if (!parsed.managedContainer && env.ROOST_TENANT_ROUTE_KEY !== undefined) {
    throw new Error("ROOST_TENANT_ROUTE_KEY requires ROOST_MANAGED_CONTAINER=1");
  }
  if (!parsed.managedContainer && env.ROOST_SAAS_AUTH_VERIFY_KEY_FILE !== undefined) {
    throw new Error("ROOST_SAAS_AUTH_VERIFY_KEY_FILE requires ROOST_MANAGED_CONTAINER=1");
  }
  if (parsed.managedContainer) {
    if (!parsed.saasMode) {
      throw new Error("ROOST_MANAGED_CONTAINER=1 requires ROOST_SAAS_MODE=1");
    }
    if (!parsed.instanceId || !CANONICAL_UUID_RE.test(parsed.instanceId)) {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires ROOST_COORDINATOR_INSTANCE_ID to be a canonical lowercase UUID",
      );
    }
    if (!isTenantRouteKey(parsed.tenantRouteKey)) {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires ROOST_TENANT_ROUTE_KEY to be exactly 64 lowercase hex characters",
      );
    }
    if (!parsed.saasAuthVerifyKeyPath?.trim()) {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires ROOST_SAAS_AUTH_VERIFY_KEY_FILE",
      );
    }
    if (!isAbsolute(parsed.saasAuthVerifyKeyPath)) {
      throw new Error("ROOST_SAAS_AUTH_VERIFY_KEY_FILE must be an absolute path");
    }
    if (parsed.bind !== "127.0.0.1:4103") {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires ROOST_COORDINATOR_BIND=127.0.0.1:4103",
      );
    }
    if (parsed.publicBind !== "0.0.0.0:4104") {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires ROOST_PUBLIC_BIND=0.0.0.0:4104",
      );
    }
    if (
      env.ROOST_WEB_PUBLIC_URL !== MANAGED_WEB_PUBLIC_ORIGIN
      || parsed.webPublicUrl !== MANAGED_WEB_PUBLIC_ORIGIN
    ) {
      throw new Error(
        `ROOST_MANAGED_CONTAINER=1 requires ROOST_WEB_PUBLIC_URL=${MANAGED_WEB_PUBLIC_ORIGIN}`,
      );
    }
    if (
      env.ROOST_CF_ACCESS_TEAM_DOMAIN !== undefined
      || env.ROOST_CF_ACCESS_AUD !== undefined
    ) {
      throw new Error("ROOST_MANAGED_CONTAINER=1 forbids Cloudflare Access settings");
    }
    if (
      env.ROOST_TLS_CERT_PATH !== undefined
      || env.ROOST_TLS_KEY_PATH !== undefined
    ) {
      throw new Error("ROOST_MANAGED_CONTAINER=1 forbids direct TLS settings");
    }
    if (env.ROOST_COORDINATOR_PUBLIC_URL !== undefined) {
      throw new Error("ROOST_MANAGED_CONTAINER=1 forbids ROOST_COORDINATOR_PUBLIC_URL");
    }
    if (
      env.ROOST_RESEND_API_KEY !== undefined
      || env.ROOST_EMAIL_OUTBOX_KEY !== undefined
    ) {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires Resend and email outbox secrets through *_FILE settings",
      );
    }
    if (
      env.ROOST_RESEND_API_KEY_FILE === undefined
      || env.ROOST_EMAIL_OUTBOX_KEY_FILE === undefined
      || providedEmailSettingCount !== emailSettings.length
    ) {
      throw new Error(
        "ROOST_MANAGED_CONTAINER=1 requires complete file-backed email settings",
      );
    }
  }

  const loopbackBind = /^127\.0\.0\.1:[1-9]\d{0,4}$/;
  const checkedPort = (bind: string, envName: string): number => {
    if (!loopbackBind.test(bind)) throw new Error(`${envName} must use 127.0.0.1:<port>`);
    const port = Number(bind.slice(bind.lastIndexOf(":") + 1));
    if (port > 65535) throw new Error(`${envName} port must be 1-65535`);
    return port;
  };

  if (parsed.trustProxy) checkedPort(parsed.bind, "ROOST_COORDINATOR_BIND");

  const accessSettings = [parsed.cfAccessTeamDomain, parsed.cfAccessAud];
  const providedAccessSettings = accessSettings.filter((value) => value !== undefined);
  if (providedAccessSettings.length === 1) {
    throw new Error(
      "ROOST_CF_ACCESS_TEAM_DOMAIN and ROOST_CF_ACCESS_AUD must be configured together",
    );
  }
  if (parsed.saasMode && providedAccessSettings.length !== 0) {
    throw new Error(
      "ROOST_SAAS_MODE=1 cannot be combined with ROOST_CF_ACCESS_TEAM_DOMAIN or ROOST_CF_ACCESS_AUD",
    );
  }
  const accessConfigured = providedAccessSettings.length === accessSettings.length;
  if (accessConfigured) {
    if (!/^[a-z0-9][a-z0-9-]*\.cloudflareaccess\.com$/.test(parsed.cfAccessTeamDomain!)) {
      throw new Error("ROOST_CF_ACCESS_TEAM_DOMAIN must be <team>.cloudflareaccess.com");
    }
    if (!/^[0-9a-f]{64}$/.test(parsed.cfAccessAud!)) {
      throw new Error("ROOST_CF_ACCESS_AUD must be the 64-hex Application Audience tag");
    }
  }

  if (parsed.publicBind) {
    if (!parsed.webPublicUrl) {
      throw new Error("ROOST_PUBLIC_BIND requires ROOST_WEB_PUBLIC_URL");
    }
    if (!parsed.saasMode && !accessConfigured) {
      throw new Error(
        "ROOST_PUBLIC_BIND without Cloudflare Access requires ROOST_SAAS_MODE=1",
      );
    }
    if (!parsed.trustProxy) {
      throw new Error("ROOST_PUBLIC_BIND requires ROOST_TRUST_PROXY=1");
    }
    if (!parsed.managedContainer) checkedPort(parsed.publicBind, "ROOST_PUBLIC_BIND");
    if (parsed.publicBind === parsed.bind) {
      throw new Error("ROOST_PUBLIC_BIND must differ from ROOST_COORDINATOR_BIND");
    }
    if (parsed.relaxedCsp) {
      throw new Error("ROOST_PUBLIC_BIND cannot be combined with ROOST_RELAXED_CSP=1");
    }
    if (parsed.publicUrl && parsed.publicUrl === parsed.webPublicUrl) {
      throw new Error(
        "ROOST_COORDINATOR_PUBLIC_URL must differ from the browser-only ROOST_WEB_PUBLIC_URL",
      );
    }
  }

  for (const origin of parsed.corsAllowedOrigins) {
    let url: URL;
    try {
      url = new URL(origin);
    } catch {
      throw new Error(`ROOST_CORS_ALLOWED_ORIGINS contains an invalid origin: ${origin}`);
    }
    if (!["http:", "https:"].includes(url.protocol) || url.origin !== origin) {
      throw new Error(`ROOST_CORS_ALLOWED_ORIGINS entries must be bare HTTP(S) origins: ${origin}`);
    }
  }

  for (const origin of parsed.pushAllowedOrigins) {
    let url: URL;
    try {
      url = new URL(origin);
    } catch {
      throw new Error(`ROOST_PUSH_ALLOWED_ORIGINS contains an invalid origin: ${origin}`);
    }
    if (url.protocol !== "https:" || url.origin !== origin) {
      throw new Error(`ROOST_PUSH_ALLOWED_ORIGINS entries must be exact bare HTTPS origins: ${origin}`);
    }
  }
  return parsed;
}
