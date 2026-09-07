// Worker Zod configuration and environment loader.
// Restore enablement is parsed eagerly and rejected before boot work starts;
// the one-shot bootstrap token is cleared after enrollment.
// Called by main boot, coordinator clients, and Windows enrollment.

import { z } from "zod";
import { workerDataDir, workerLogDir } from "@roost/shared/paths";
import { join } from "node:path";
import { hostname } from "node:os";

/** Owned by service-definition-env.ts, which has no module-scope host-path
 * resolution: the config read below and the boot-time erasure there must
 * address the same installed entry. */
import { KEEPER_FORCE_LIVE_RETIRE_ENV } from "./service-definition-env.ts";

export const WorkerConfig = z.object({
  coordinatorUrl: z.string().url(),
  bootstrapToken: z.string().optional(),      // one-shot first-boot
  label: z.string().min(1),
  agentConversationRestore: z.boolean().default(false),
  keeperForceLiveRetire: z.boolean().default(false),
  // Resolved lazily: this module is imported by Windows enrollment paths that
  // validate env BEFORE a host layout exists, and an eager default made the
  // import itself throw "LOCALAPPDATA or USERPROFILE is required" ahead of the
  // validation error the caller was about to report. withDefaults supplies the
  // real value from the caller's env.
  logDir: z.string().default(() => workerLogDir()),
  // path to coordinator_ed25519.key (the worker's own JWT-signing key)
  workerKeyPath: z.string(),
});
export type WorkerConfig = z.infer<typeof WorkerConfig>;

// Default state dir = RoostWorkerV2 (v2-isolated). Legacy Rust worker uses
// idea-worker/; v2 must NOT share paths or it will clobber the legacy
// raw-32-byte seed file during auto-regen and crash-loop legacy.
// The service installer always passes ROOST_WORKER_DATA_DIR explicitly.
function withDefaults(
  env: Record<string, string | undefined>,
  platform: NodeJS.Platform,
): Record<string, unknown> {
  const SUPPORT = workerDataDir(env);
  // Worker has no inbound surface post phase-24d-1; reachableAddr /
  // wsListenPort / wsScheme / tls* / coordVerifyingKeyPath dropped
  // entirely in phase-25e.
  return {
    // Dev fallback only — install.sh always passes the tailnet FQDN
    // explicitly via the plist. Bare `bun apps/worker/src/main.ts` then
    // dials the local coord instead of throwing a Zod url() error.
    coordinatorUrl: env.ROOST_COORDINATOR_URL ?? "http://localhost:4102",
    bootstrapToken: env.ROOST_BOOTSTRAP_TOKEN,
    agentConversationRestore: parseAgentConversationRestore(
      env.ROOST_AGENT_CONVERSATION_RESTORE,
      platform,
    ),
    keeperForceLiveRetire: parseKeeperForceLiveRetire(
      env[KEEPER_FORCE_LIVE_RETIRE_ENV],
    ),
    // Prefer the actual machine hostname from node:os over env.HOSTNAME,
    // which isn't set on macOS by default — that was the regression
    // behind every worker registering as the literal string "worker"
    // in the sidebar.
    label: env.ROOST_WORKER_LABEL ?? hostname() ?? env.HOSTNAME ?? "worker",
    logDir: workerLogDir(env),
    workerKeyPath: env.ROOST_WORKER_KEY_PATH ??
      join(SUPPORT, "coordinator_ed25519.key"),
  };
}

export function loadWorkerConfig(
  env: Record<string, string | undefined> = process.env as Record<string, string | undefined>,
  platform: NodeJS.Platform = process.platform,
): WorkerConfig {
  return WorkerConfig.parse(withDefaults(env, platform));
}

function parseAgentConversationRestore(
  value: string | undefined,
  platform: NodeJS.Platform,
): boolean {
  if (value === undefined || value === "0") return false;
  if (value !== "1") {
    throw new Error("ROOST_AGENT_CONVERSATION_RESTORE must be exactly 0 or 1");
  }
  if (platform === "win32") {
    throw new Error("ROOST_AGENT_CONVERSATION_RESTORE=1 is unsupported on Windows");
  }
  return true;
}

/** Operator authorization to discard a keeper the worker can neither adopt nor
 * prove empty. It ends every PTY that keeper hosts, so an unrecognized value
 * is a hard error rather than a silent truthy arming. */
function parseKeeperForceLiveRetire(value: string | undefined): boolean {
  if (value === undefined || value === "0") return false;
  if (value !== "1") {
    throw new Error(`${KEEPER_FORCE_LIVE_RETIRE_ENV} must be exactly 0 or 1`);
  }
  return true;
}
