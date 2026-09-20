// Small browser-runtime policies shared by terminal peer election operations.
// The peer owner supplies mutable demand state; this module owns monotonic time,
// visibility, bounded awaits, and demand edge classification without transport I/O.

export interface TerminalPeerDemandState {
  readonly demandedSessions: Set<string>;
}

export function recordTerminalPeerViewDemand(
  state: TerminalPeerDemandState,
  sessionId: string,
  active: boolean,
): boolean {
  const hadDemand = state.demandedSessions.has(sessionId);
  if (active === hadDemand) return false;
  if (active) state.demandedSessions.add(sessionId);
  else state.demandedSessions.delete(sessionId);
  return true;
}

export function hasTerminalPeerDemand(
  state: TerminalPeerDemandState,
  sessionId: string,
): boolean {
  return state.demandedSessions.has(sessionId);
}
interface TerminalPeerHeartbeatState {
  heartbeatEpisode: number;
  heartbeatPending: boolean;
}

interface TerminalPeerFreshProbe {
  requireFreshProbe(): void;
}

export function beginTerminalPeerHeartbeatEpisode(
  state: TerminalPeerHeartbeatState,
  connection: TerminalPeerFreshProbe | null,
  requireFresh: boolean,
): number | null {
  if (requireFresh) {
    state.heartbeatEpisode += 1;
    connection?.requireFreshProbe();
    state.heartbeatPending = false;
  }
  if (state.heartbeatPending) return null;
  state.heartbeatPending = true;
  return state.heartbeatEpisode;
}


export function activeTerminalPeerViewCount(state: TerminalPeerDemandState): number {
  return state.demandedSessions.size;
}

export function terminalPeerPageHidden(): boolean {
  return typeof document !== "undefined" && document.visibilityState !== "visible";
}

export function terminalPeerNow(): number {
  return globalThis.performance?.now() ?? Date.now();
}

export async function terminalPeerDeadline<T>(
  promise: Promise<T>,
  timeoutMs: number,
  controller?: AbortController,
): Promise<T> {
  const { promise: timeout, reject } = Promise.withResolvers<T>();
  const timer = setTimeout(() => {
    controller?.abort();
    reject(new Error("terminal peer deadline elapsed"));
  }, timeoutMs);
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    clearTimeout(timer);
  }
}
