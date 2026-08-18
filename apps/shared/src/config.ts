// The coord Zod config schema, loaded at boot. Fails at boot if invalid (not at
// first-use). R0.12. The worker's schema + loader live in apps/worker/src/config.ts.

import { z } from "zod";
import { resolveTailnetDnsName } from "./tailnet.ts";
import { coordDataDir, coordLogDir } from "./paths.ts";
import { join } from "node:path";

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

// ─── coord config ──────────────────────────────────────────────────────

export const CoordConfig = z.object({
  bind: z.string().default("0.0.0.0:4102"),
  dbPath: z.string(),
  authorizedKeysPath: z.string(),
  webDistPath: z.string().optional(),         // vinxi/vite build output for SPA serve
  coordKeyPath: z.string(),                    // OpenSSH ed25519 key for JWT signing
  jwtMaxAgeSecs: z.number().int().positive().default(300),
  // Age-out window for the high-volume audit_log rows (keystrokes, SPA polling).
  // Auth/pair/delete rows are never swept — see apps/coord/src/audit-retention.ts.
  auditRetentionDays: z.number().int().positive().default(90),
  corsAllowedOrigins: z.array(z.string()).default([]),
  relaxedCsp: z.boolean().default(false),
  trustProxy: z.boolean().default(false),
  publicBind: z.string().optional(),
  webPublicUrl: z.string().url().optional(),
  cfAccessTeamDomain: z.string().optional(),
  cfAccessAud: z.string().optional(),
  logDir: z.string().default(coordLogDir()),
  // Tailnet TLS via `tailscale cert <fqdn>`. When BOTH paths are set,
  // coord serves HTTPS instead of HTTP. Browsers reach the tailnet FQDN
  // over HTTPS, satisfying secure-context APIs (WebCrypto, SubtleCrypto)
  // and avoiding mixed-content when a worker serves WSS.
  tlsCertPath: z.string().optional(),
  tlsKeyPath: z.string().optional(),
  publicUrl: z.string().url().optional(),
  handoffPath: z.string(),
});
export type CoordConfig = z.infer<typeof CoordConfig>;

export function loadCoordConfig(env: Record<string, string | undefined> = process.env): CoordConfig {
  const dataDir = coordDataDir(env);
  const tailnetDnsName = resolveTailnetDnsName();
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
    relaxedCsp: env.ROOST_RELAXED_CSP === "1",
    trustProxy: env.ROOST_TRUST_PROXY === "1",
    publicBind: env.ROOST_PUBLIC_BIND,
    webPublicUrl: normalizeHttpsOrigin(env.ROOST_WEB_PUBLIC_URL, "ROOST_WEB_PUBLIC_URL"),
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

  const loopbackBind = /^127\.0\.0\.1:[1-9]\d{0,4}$/;
  const checkedPort = (bind: string, envName: string): number => {
    if (!loopbackBind.test(bind)) throw new Error(`${envName} must use 127.0.0.1:<port>`);
    const port = Number(bind.slice(bind.lastIndexOf(":") + 1));
    if (port > 65535) throw new Error(`${envName} port must be 1-65535`);
    return port;
  };

  if (parsed.trustProxy) checkedPort(parsed.bind, "ROOST_COORDINATOR_BIND");
  if (parsed.publicBind) {
    if (!parsed.cfAccessTeamDomain || !parsed.cfAccessAud || !parsed.webPublicUrl) {
      throw new Error(
        "ROOST_PUBLIC_BIND requires ROOST_CF_ACCESS_TEAM_DOMAIN, ROOST_CF_ACCESS_AUD and ROOST_WEB_PUBLIC_URL",
      );
    }
    if (!parsed.trustProxy) {
      throw new Error("ROOST_PUBLIC_BIND requires ROOST_TRUST_PROXY=1 for the private tailscale-serve listener");
    }
    checkedPort(parsed.publicBind, "ROOST_PUBLIC_BIND");
    if (parsed.publicBind === parsed.bind) {
      throw new Error("ROOST_PUBLIC_BIND must differ from ROOST_COORDINATOR_BIND");
    }
    if (!/^[a-z0-9][a-z0-9-]*\.cloudflareaccess\.com$/.test(parsed.cfAccessTeamDomain)) {
      throw new Error("ROOST_CF_ACCESS_TEAM_DOMAIN must be <team>.cloudflareaccess.com");
    }
    if (!/^[0-9a-f]{64}$/.test(parsed.cfAccessAud)) {
      throw new Error("ROOST_CF_ACCESS_AUD must be the 64-hex Application Audience tag");
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
  return parsed;
}
