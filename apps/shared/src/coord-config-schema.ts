// Defines the coordinator configuration shape shared by boot-time parsing and callers.
// Keeping the Zod schema separate lets environment loading focus on normalization and
// cross-field policy while preserving the public CoordConfig value and type.

import { z } from "zod";
import { coordLogDir } from "./paths.ts";

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
  pushAllowedOrigins: z.array(z.string()).default([]),
  relaxedCsp: z.boolean().default(false),
  trustProxy: z.boolean().default(false),
  publicBind: z.string().optional(),
  webPublicUrl: z.string().url().optional(),
  cfAccessTeamDomain: z.string().optional(),
  saasMode: z.boolean().default(false),
  managedContainer: z.boolean().default(false),
  instanceId: z.string().optional(),
  tenantRouteKey: z.string().optional(),
  saasAuthVerifyKeyPath: z.string().optional(),
  resendEndpoint: z.string().url().optional(),
  resendApiKey: z.string().min(1).optional(),
  emailFrom: z.string().min(1).optional(),
  emailOutboxKey: z.string().min(1).optional(),
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
