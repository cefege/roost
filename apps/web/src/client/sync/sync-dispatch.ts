// Per-session terminal presence metadata dispatch. Screen continuity is owned
// by terminal-stream.ts and therefore never depends on a mounted component.

type PresenceHandler = (msg: unknown) => void;

const presenceHandlers = new Map<string, PresenceHandler>();

export function registerPresenceHandler(
  sessionId: string,
  handler: PresenceHandler,
): () => void {
  presenceHandlers.set(sessionId, handler);
  return () => {
    if (presenceHandlers.get(sessionId) === handler) presenceHandlers.delete(sessionId);
  };
}

export function _dispatchPresence(sessionId: string, data: unknown): void {
  presenceHandlers.get(sessionId)?.(data);
}
