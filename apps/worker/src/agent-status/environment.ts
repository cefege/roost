import { homedir } from "node:os";
import { join } from "node:path";

export function defaultAgentReportSocketPath(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): string {
  return env.ROOST_AGENT_SOCKET_PATH || join(home, ".roost", "agent-report.sock");
}

export function withAgentStatusEnvironment(
  base: Record<string, string>,
  sessionId: string,
  env: NodeJS.ProcessEnv = process.env,
): Record<string, string> {
  return {
    ...base,
    ROOST_AGENT_SOCKET_PATH: defaultAgentReportSocketPath(env),
    ROOST_SESSION_ID: sessionId,
  };
}
