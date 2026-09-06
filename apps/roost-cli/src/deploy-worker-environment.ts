// Worker service-install environment construction shared by remote and local
// POSIX deploys. Installed explicit settings seed each host independently;
// target-release fields are replaced so no prior worktree or token leaks.

import { posixShellQuote } from "@roost/shared/shell-quote";

const CONVERSATION_RESTORE_ENV = "ROOST_AGENT_CONVERSATION_RESTORE";

export function workerInstallEnvironmentValues(
  installed: Readonly<Record<string, string>>,
  overrides: Readonly<Record<string, string | undefined>>,
  gitSha: string,
  ambient: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  const values: Record<string, string> = { ...installed };
  for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
    delete values[key];
  }
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete values[key];
    else values[key] = value;
  }
  const conversationRestore = installed[CONVERSATION_RESTORE_ENV]
    ?? overrides[CONVERSATION_RESTORE_ENV]
    ?? ambient[CONVERSATION_RESTORE_ENV];
  if (conversationRestore === undefined) delete values[CONVERSATION_RESTORE_ENV];
  else values[CONVERSATION_RESTORE_ENV] = conversationRestore;
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
