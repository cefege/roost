// Worker service installers use this policy to forward POSIX overrides and
// reject unsupported Windows enablement before creating service state.
// The worker configuration remains the runtime parser for exact 0/1 values.

import { homedir } from "node:os";

const CONVERSATION_RESTORE_ENV = "ROOST_AGENT_CONVERSATION_RESTORE";

function posixConversationRestoreEnvironment(
  environment: Readonly<Record<string, string>> | undefined,
  ambient: Readonly<Record<string, string | undefined>> = process.env,
): Record<string, string> {
  const value = environment?.[CONVERSATION_RESTORE_ENV]
    ?? ambient[CONVERSATION_RESTORE_ENV];
  return value === undefined ? {} : { [CONVERSATION_RESTORE_ENV]: value };
}

export function buildPosixWorkerInstallEnvironment(options: {
  execPath: string;
  coordUrl: string;
  bootstrapToken?: string;
  gitSha: string;
  env?: Record<string, string>;
}): Record<string, string> {
  return {
    ...posixConversationRestoreEnvironment(options.env),
    ROOST_EXEC_BIN: options.execPath,
    ROOST_WORKDIR: homedir(),
    ROOST_COORDINATOR_URL: options.coordUrl,
    GIT_SHA: options.gitSha,
    ...(options.bootstrapToken
      ? { ROOST_BOOTSTRAP_TOKEN: options.bootstrapToken }
      : {}),
  };
}

export function assertWindowsConversationRestoreDisabled(
  environment: Readonly<Record<string, string>>,
): void {
  const matchingEntries = Object.entries(environment).filter(
    ([key]) => key.toUpperCase() === CONVERSATION_RESTORE_ENV,
  );
  if (matchingEntries.length > 1) {
    throw new Error("Windows worker environment contains duplicate conversation restore settings");
  }
  const value = matchingEntries[0]?.[1];
  if (value === "1") {
    throw new Error("ROOST_AGENT_CONVERSATION_RESTORE=1 is unsupported on Windows");
  }
  if (value !== undefined && value !== "0") {
    throw new Error("ROOST_AGENT_CONVERSATION_RESTORE must be exactly 0 or 1");
  }
}
