// Service-install environment construction shared by Linux and macOS worker
// deploys. Installed identity seeds each host independently; target-release
// fields are always replaced so no prior worktree or token leaks forward.

import { posixShellQuote } from "@roost/shared/shell-quote";

export function workerInstallEnvironment(
  installed: Readonly<Record<string, string>>,
  overrides: Readonly<Record<string, string | undefined>>,
  gitSha: string,
): string {
  const values: Record<string, string> = { ...installed };
  for (const key of ["GIT_SHA", "ROOST_GIT_SHA", "ROOST_WORKDIR", "ROOST_EXEC_BIN", "ROOST_BOOTSTRAP_TOKEN"]) {
    delete values[key];
  }
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) delete values[key];
    else values[key] = value;
  }
  values.GIT_SHA = gitSha;
  return Object.entries(values)
    .filter(([key]) => key === "GIT_SHA" || /^ROOST_[A-Z_]+$/.test(key))
    .map(([key, value]) => `${key}=${posixShellQuote(value)}`)
    .join(" ");
}
