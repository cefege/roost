// Loads coordinator configuration from environment variables during boot.
// It normalizes external input and enforces the cross-field policy the listener depends on.
// The declarative configuration shape lives separately in coord-config-schema.ts.

import { join } from "node:path";
import { CoordConfig, DEFAULT_COORDINATOR_BIND } from "./coord-config-schema.ts";
import { coordDataDir } from "./paths.ts";

export { CoordConfig, DEFAULT_COORDINATOR_BIND };

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

export function loadCoordConfig(env: Record<string, string | undefined> = process.env): CoordConfig {
  const hasCfAccessTeamDomain = env.ROOST_CF_ACCESS_TEAM_DOMAIN !== undefined;
  const hasCfAccessAud = env.ROOST_CF_ACCESS_AUD !== undefined;
  if (hasCfAccessTeamDomain !== hasCfAccessAud) {
    throw new Error("ROOST_CF_ACCESS_TEAM_DOMAIN and ROOST_CF_ACCESS_AUD must be set together");
  }
  const dataDir = coordDataDir(env);
  const parsed = CoordConfig.parse({
    bind: env.ROOST_COORDINATOR_BIND,
    dbPath: env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db"),
    authorizedKeysPath: env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(dataDir, "authorized_keys.roost"),
    webDistPath: env.ROOST_WEB_DIST_PATH,
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
    cfAccessTeamDomain: env.ROOST_CF_ACCESS_TEAM_DOMAIN,
    cfAccessAud: env.ROOST_CF_ACCESS_AUD,
    webPublicUrl: normalizeHttpsOrigin(env.ROOST_WEB_PUBLIC_URL, "ROOST_WEB_PUBLIC_URL"),
    logDir: env.ROOST_COORDINATOR_LOG_DIR,
    publicUrl: normalizeHttpsOrigin(env.ROOST_COORDINATOR_PUBLIC_URL, "ROOST_COORDINATOR_PUBLIC_URL"),
  });

  // Trusting X-Forwarded-For makes the caller origin attacker-controlled unless every
  // request arrives through the operator's front door, so the socket must stay on loopback.
  if (parsed.trustProxy) {
    if (!/^127\.0\.0\.1:[1-9]\d{0,4}$/.test(parsed.bind)) {
      throw new Error("ROOST_COORDINATOR_BIND must use 127.0.0.1:<port>");
    }
    if (Number(parsed.bind.slice(parsed.bind.lastIndexOf(":") + 1)) > 65535) {
      throw new Error("ROOST_COORDINATOR_BIND port must be 1-65535");
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
