// Owns the worker-browse terminal launch continuation.
// WorkerBrowsePage supplies navigation while this module fences every asynchronous
// boundary against a credential cutover before publishing authenticated UI state.

import type { Session, WorkerFp } from "@roost/shared/wire";
import {
  captureAuthResourceToken,
  isCurrentAuthResourceToken,
} from "../store/auth-boundary.ts";
import { addToast } from "../store/toastStore.ts";
import { maybeAutoLaunchAgent, spawnShell, waitForSession } from "../lib/spawnSession.ts";
import { terminalHref } from "../lib/terminalHref.ts";
import { pushRecent } from "../lib/sidebarRecent.ts";
import { sessionHref } from "../routes.ts";

export interface _WorkerBrowseLaunchDependencies {
  readonly captureAuthResourceToken: typeof captureAuthResourceToken;
  readonly isCurrentAuthResourceToken: typeof isCurrentAuthResourceToken;
  readonly spawnShell: (workerFp: WorkerFp, path: string) => Promise<string>;
  readonly waitForSession: (sessionId: string) => Promise<Session | null>;
  readonly pushRecent: (sessionId: string) => void;
  readonly maybeAutoLaunchAgent: (sessionId: string) => void;
  readonly terminalHref: (session: Session) => string;
  readonly sessionHref: (sessionId: string) => string;
  readonly addToast: typeof addToast;
}

const defaultDependencies: _WorkerBrowseLaunchDependencies = {
  captureAuthResourceToken,
  isCurrentAuthResourceToken,
  spawnShell,
  waitForSession,
  pushRecent,
  maybeAutoLaunchAgent,
  terminalHref,
  sessionHref,
  addToast,
};

export async function launchWorkerBrowseTerminal(
  workerFp: WorkerFp,
  path: string,
  navigate: (href: string) => void,
  dependencies: _WorkerBrowseLaunchDependencies = defaultDependencies,
): Promise<void> {
  const authToken = dependencies.captureAuthResourceToken();
  try {
    const sessionId = await dependencies.spawnShell(workerFp, path);
    if (!dependencies.isCurrentAuthResourceToken(authToken)) return;

    const session = await dependencies.waitForSession(sessionId);
    if (!dependencies.isCurrentAuthResourceToken(authToken)) return;

    dependencies.pushRecent(sessionId);
    dependencies.maybeAutoLaunchAgent(sessionId);
    navigate(session ? dependencies.terminalHref(session) : dependencies.sessionHref(sessionId));
  } catch (error) {
    if (!dependencies.isCurrentAuthResourceToken(authToken)) return;
    const message = error instanceof Error ? error.message : String(error);
    dependencies.addToast(`New terminal failed: ${message}`, "err");
  }
}
