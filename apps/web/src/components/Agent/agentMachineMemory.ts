// Browser-local memory of the last machine used in the /agent pane, so the bare
// /agent route lands back on it instead of an arbitrary worker.
// This is the ONLY thing the agent surface persists: transcripts, tool results,
// cursors and permission verdicts are Mecatl's and are never written here,
// to rootStore, or to Sync. Read by AgentPane; written when a machine resolves.

const LAST_AGENT_MACHINE_KEY = "roost.lastAgentMachine";

export function rememberAgentMachine(workerFp: string): void {
  try {
    localStorage.setItem(LAST_AGENT_MACHINE_KEY, workerFp);
  } catch { /* quota / privacy mode */ }
}

export function lastAgentMachine(): string | null {
  try {
    return localStorage.getItem(LAST_AGENT_MACHINE_KEY);
  } catch { return null; }
}
