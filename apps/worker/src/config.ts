// Worker config loader. Reads env vars → WorkerConfig Zod schema (apps/shared/src/config.ts).
// Fails at boot if invalid (not at first-use). R0.12.
// Bootstrap token is one-shot: cleared from env after redeem.
// Callers: main.ts, coord-client.ts.

import { WorkerConfig, type WorkerConfig as WorkerConfigType } from "@roost/shared";
import { join } from "node:path";
import { homedir, hostname } from "node:os";

// Default state dir = RoostWorkerV2 (v2-isolated). Legacy Rust worker uses
// idea-worker/; v2 must NOT share paths or it will clobber the legacy
// raw-32-byte seed file during auto-regen and crash-loop legacy.
// LaunchAgent install.sh always passes ROOST_WORKER_DATA_DIR explicitly.
function defaultSupportDir(env: Record<string, string | undefined>): string {
  return env.ROOST_WORKER_DATA_DIR
    ?? join(homedir(), "Library", "Application Support", "RoostWorkerV2");
}

function withDefaults(env: Record<string, string | undefined>): Record<string, unknown> {
  const SUPPORT = defaultSupportDir(env);
  // Worker has no inbound surface post phase-24d-1; reachableAddr /
  // wsListenPort / wsScheme / tls* / coordVerifyingKeyPath dropped
  // entirely in phase-25e.
  return {
    // Dev fallback only — install.sh always passes the tailnet FQDN
    // explicitly via the plist. Bare `bun apps/worker/src/main.ts` then
    // dials the local coord instead of throwing a Zod url() error.
    coordinatorUrl: env.ROOST_COORDINATOR_URL ?? "http://localhost:4102",
    bootstrapToken: env.ROOST_BOOTSTRAP_TOKEN,
    // Prefer the actual machine hostname from node:os over env.HOSTNAME,
    // which isn't set on macOS by default — that was the regression
    // behind every worker registering as the literal string "worker"
    // in the sidebar.
    label: env.ROOST_WORKER_LABEL ?? hostname() ?? env.HOSTNAME ?? "worker",
    logDir: env.ROOST_WORKER_LOG_DIR ?? join(homedir(), "Library", "Logs", "RoostWorker"),
    workerKeyPath: env.ROOST_WORKER_KEY_PATH ??
      join(SUPPORT, "coordinator_ed25519.key"),
  };
}

export function loadWorkerConfig(env: Record<string, string | undefined> = process.env as Record<string, string | undefined>): WorkerConfigType {
  return WorkerConfig.parse(withDefaults(env));
}
