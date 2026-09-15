// Worker service-install environment construction shared by remote and local
// POSIX deploys. Installed explicit settings seed each host independently;
// target-release fields are replaced so no prior worktree or token leaks.

import { posixShellQuote } from "@roost/shared/shell-quote";
import {
  AGENT_CONVERSATION_RESTORE_ENV as CONVERSATION_RESTORE_ENV,
  KEEPER_FORCE_LIVE_RETIRE_ENV,
} from "@roost/shared/worker-service-env";
import { DEPLOY_HOST_LOCAL_ENV_KEYS } from "./deploy-plist-env.ts";

export { KEEPER_FORCE_LIVE_RETIRE_ENV };

export function workerInstallEnvironmentValues(
  installed: Readonly<Record<string, string>>,
  overrides: Readonly<Record<string, string | undefined>>,
  gitSha: string,
  ambient: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  const values: Record<string, string> = { ...installed };
  // The retire authorization is stripped like the bootstrap token: a retained
  // flag would silently authorize discarding a keeper's live channels on every
  // later activation, so only the deploy that was given it carries it. This is
  // the second half of that guarantee — the worker spends the value out of its
  // own service definition at boot, which is what covers a plain restart.
  for (const key of [
    "GIT_SHA",
    "ROOST_GIT_SHA",
    "ROOST_WORKDIR",
    // Points into a release directory, and settlement deletes the release it
    // replaced: a retained value leaves the local UI door serving 404s. Each
    // deploy stamps the dist it just built instead.
    "ROOST_WEB_DIST_PATH",
    "ROOST_EXEC_BIN",
    "ROOST_BOOTSTRAP_TOKEN",
    KEEPER_FORCE_LIVE_RETIRE_ENV,
  ]) {
    delete values[key];
  }
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete values[key];
    else values[key] = value;
  }
  // Settings whose value belongs to the target machine: the installed choice is
  // the last decision an operator made ON that box, so it outranks this shell,
  // which only seeds a first install. Absent everywhere the key is removed
  // rather than defaulted, leaving the worker's own config default in charge.
  for (const key of [CONVERSATION_RESTORE_ENV, ...DEPLOY_HOST_LOCAL_ENV_KEYS]) {
    const resolved = installed[key] ?? overrides[key] ?? ambient[key];
    if (resolved === undefined) delete values[key];
    else values[key] = resolved;
  }
  values.GIT_SHA = gitSha;
  return values;
}

export function workerInstallEnvironment(
  installed: Readonly<Record<string, string>>,
  overrides: Readonly<Record<string, string | undefined>>,
  gitSha: string,
): string {
  return Object.entries(workerInstallEnvironmentValues(installed, overrides, gitSha))
    .filter(([key]) => key === "GIT_SHA" || /^ROOST_[A-Z_]+$/.test(key))
    .map(([key, value]) => `${key}=${posixShellQuote(value)}`)
    .join(" ");
}
