// One coordinator→worker screen-snapshot request for the stream a session owns
// right now. TerminalViewStreamController delegates the screen hub's
// requestSnapshot callback here. Every exit either sends on the session's
// current route or publishes that session unavailable, and each resumed await
// re-checks stream identity, so a snapshot belonging to a superseded stream can
// neither send nor mark the live one down.
import type { TerminalUnavailablePolicy } from "@roost/shared/terminal-view";
import type { TerminalGeometry } from "@roost/shared/viewport";
import type {
  TerminalStreamRoute,
  TerminalViewStreamControllerOptions,
} from "./terminal-view-stream-controller-types.ts";

/** The controller state one snapshot request reads, narrowed to what it needs:
 * the live session table for identity re-checks, the route/send seams, and the
 * controller's own unavailable publication. */
export interface TerminalSnapshotRequestPort {
  sessions: ReadonlyMap<string, { effective: TerminalGeometry | null; streamId: string }>;
  options: Pick<
    TerminalViewStreamControllerOptions,
    "resolveRoute" | "sendSnapshot" | "repairUnownedSession"
  >;
  unavailable(sessionId: string, message: string, policy?: TerminalUnavailablePolicy): void;
}

export async function requestTerminalScreenSnapshot(
  sessionId: string,
  streamId: string,
  port: TerminalSnapshotRequestPort,
): Promise<void> {
  const session = port.sessions.get(sessionId);
  // A session the controller never minimized belongs to a worker that owns its
  // own terminal views; its repair leaves through that worker, not the desire
  // loop, which holds no stream for it to resolve.
  if (!session) {
    port.options.repairUnownedSession?.(sessionId, streamId);
    return;
  }
  const isCurrentRequest = () => port.sessions.get(sessionId) === session
    && session.effective !== null && session.streamId === streamId;
  if (!isCurrentRequest()) return;
  let route: TerminalStreamRoute | null;
  try {
    route = await port.options.resolveRoute(sessionId);
  } catch {
    if (isCurrentRequest()) port.unavailable(sessionId, "snapshot request could not reach worker");
    return;
  }
  if (!isCurrentRequest()) return;
  if (!route) {
    port.unavailable(sessionId, "snapshot request has no worker route", "route");
    return;
  }
  try {
    if (!port.options.sendSnapshot(route.workerFp, sessionId, streamId)) {
      if (isCurrentRequest()) port.unavailable(sessionId, "snapshot request could not reach worker");
    }
  } catch {
    if (isCurrentRequest()) port.unavailable(sessionId, "snapshot request could not reach worker");
  }
}
