// One Zod config schema loaded by coord + worker at boot. Fails at
// boot if invalid (not at first-use). R0.12.

import { z } from "zod";
import { homedir } from "node:os";
import { join } from "node:path";

// Default coord data dir — MUST match apps/coord/scripts/install.sh
// (DATA_DIR="$HOME/Library/Application Support/RoostCoordinatorV2").
// install.sh still passes these paths explicitly via the plist; these
// defaults only kick in for a bare `bun apps/coord/src/main.ts` run so
// it no longer dies on an opaque Zod "required" error before printing
// anything useful.
function coordDataDir(env: Record<string, string | undefined>): string {
  return join(env.HOME ?? homedir(), "Library", "Application Support", "RoostCoordinatorV2");
}

// ─── coord config ──────────────────────────────────────────────────────

export const CoordConfig = z.object({
  bind: z.string().default("0.0.0.0:4102"),
  dbPath: z.string(),
  authorizedKeysPath: z.string(),
  webDistPath: z.string().optional(),         // vinxi/vite build output for SPA serve
  coordKeyPath: z.string(),                    // OpenSSH ed25519 key for JWT signing
  jwtMaxAgeSecs: z.number().int().positive().default(300),
  corsAllowedOrigins: z.array(z.string()).default([]),
  relaxedCsp: z.boolean().default(false),
  logDir: z.string().default("~/Library/Logs/RoostCoord"),
  // Tailnet TLS via `tailscale cert <fqdn>`. When BOTH paths are set,
  // coord serves HTTPS instead of HTTP. Browsers reach the tailnet FQDN
  // over HTTPS, satisfying secure-context APIs (WebCrypto, SubtleCrypto)
  // and avoiding mixed-content when a worker serves WSS.
  tlsCertPath: z.string().optional(),
  tlsKeyPath: z.string().optional(),
});
export type CoordConfig = z.infer<typeof CoordConfig>;

export function loadCoordConfig(env: Record<string, string | undefined> = process.env): CoordConfig {
  const dataDir = coordDataDir(env);
  return CoordConfig.parse({
    bind: env.ROOST_COORDINATOR_BIND,
    dbPath: env.ROOST_COORDINATOR_DB ?? join(dataDir, "coordinator_v2.db"),
    authorizedKeysPath: env.ROOST_COORDINATOR_AUTHORIZED_KEYS ?? join(dataDir, "authorized_keys.roost"),
    webDistPath: env.ROOST_WEB_DIST_PATH,
    coordKeyPath: env.ROOST_COORDINATOR_KEY_PATH ?? join(dataDir, "ssh_ed25519.key"),
    jwtMaxAgeSecs: env.ROOST_COORDINATOR_JWT_MAX_AGE_SECS
      ? Number(env.ROOST_COORDINATOR_JWT_MAX_AGE_SECS)
      : undefined,
    corsAllowedOrigins: env.ROOST_CORS_ALLOWED_ORIGINS
      ? env.ROOST_CORS_ALLOWED_ORIGINS.split(",").map((s) => s.trim()).filter(Boolean)
      : [],
    relaxedCsp: env.ROOST_RELAXED_CSP === "1",
    logDir: env.ROOST_COORDINATOR_LOG_DIR,
    tlsCertPath: env.ROOST_TLS_CERT_PATH,
    tlsKeyPath: env.ROOST_TLS_KEY_PATH,
  });
}

// ─── worker config ─────────────────────────────────────────────────────

export const WorkerConfig = z.object({
  coordinatorUrl: z.string().url(),
  bootstrapToken: z.string().optional(),      // one-shot first-boot
  label: z.string().min(1),
  logDir: z.string().default("~/Library/Logs/RoostWorker"),
  // path to coordinator_ed25519.key (the worker's own JWT-signing key)
  workerKeyPath: z.string(),
});
export type WorkerConfig = z.infer<typeof WorkerConfig>;
