// Defines the coordinator configuration shape shared by boot-time parsing and callers.
// Keeping the Zod schema separate lets environment loading focus on normalization and
// cross-field policy while preserving the public CoordConfig value and type.

import { z } from "zod";
import { coordLogDir } from "./paths.ts";

/** The bind an unset `ROOST_COORDINATOR_BIND` resolves to. Loopback, because the
 * coordinator serves plaintext and must never expose the dashboard on every
 * interface by default; `roost dev` opts out explicitly. Callers that need to
 * reach a bare coordinator import this rather than restating the port. */
export const DEFAULT_COORDINATOR_BIND = "127.0.0.1:4103";

export const CoordConfig = z.object({
  bind: z.string().default(DEFAULT_COORDINATOR_BIND),
  dbPath: z.string(),
  authorizedKeysPath: z.string(),
  webDistPath: z.string().optional(),         // vinxi/vite build output for SPA serve
  jwtMaxAgeSecs: z.number().int().positive().default(300),
  // Age-out window for the high-volume audit_log rows (keystrokes, SPA polling).
  // Auth/pair/delete rows are never swept — see apps/coord/src/audit-retention.ts.
  auditRetentionDays: z.number().int().positive().default(90),
  corsAllowedOrigins: z.array(z.string()).default([]),
  pushAllowedOrigins: z.array(z.string()).default([]),
  relaxedCsp: z.boolean().default(false),
  trustProxy: z.boolean().default(false),
  // Operator-declared browser front door. Seeds the CSP connect-src allowance and
  // the Sync WS origin allowlist; the front door itself owns TLS and DNS.
  webPublicUrl: z.string().url().optional(),
  logDir: z.string().default(coordLogDir()),
  // Coordinator identity origin for operators whose worker traffic enters through a
  // different door than the browser front door. Never derived, only declared.
  publicUrl: z.string().url().optional(),
});
export type CoordConfig = z.infer<typeof CoordConfig>;
