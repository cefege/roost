// Owns the worker-browse terminal launch continuation.
// WorkerBrowsePage supplies navigation while this module fences every asynchronous
// boundary against dashboard cutovers before publishing scoped UI state.

import type { Session, WorkerFp } from "@roost/shared/wire";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
} from "../store/dashboard-selection.ts";
import { addToast } from "../store/toastStore.ts";
import { maybeAutoLaunchAgent, spawnShell, waitForSession } from "../lib/spawnSession.ts";
import { terminalHref } from "../lib/terminalHref.ts";
import { pushRecent } from "../lib/sidebarRecent.ts";
import { sessionHref } from "../routes.ts";

export interface _WorkerBrowseLaunchDependencies {
  readonly captureDashboardResourceToken: typeof captureDashboardResourceToken;
  readonly isCurrentDashboardResourceToken: typeof isCurrentDashboardResourceToken;
  readonly spawnShell: (workerFp: WorkerFp, path: string) => Promise<string>;
  readonly waitForSession: (sessionId: string) => Promise<Session | null>;
  readonly pushRecent: (sessionId: string) => void;
  readonly maybeAutoLaunchAgent: (sessionId: string) => void;
  readonly terminalHref: (session: Session) => string;
  readonly sessionHref: (sessionId: string) => string;
  readonly addToast: typeof addToast;
}

const defaultDependencies: _WorkerBrowseLaunchDependencies = {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
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
  const dashboardToken = dependencies.captureDashboardResourceToken();
  try {
    const sessionId = await dependencies.spawnShell(workerFp, path);
    if (!dependencies.isCurrentDashboardResourceToken(dashboardToken)) return;

    const session = await dependencies.waitForSession(sessionId);
    if (!dependencies.isCurrentDashboardResourceToken(dashboardToken)) return;

    dependencies.pushRecent(sessionId);
    dependencies.maybeAutoLaunchAgent(sessionId);
    navigate(session ? dependencies.terminalHref(session) : dependencies.sessionHref(sessionId));
  } catch (error) {
    if (!dependencies.isCurrentDashboardResourceToken(dashboardToken)) return;
    const message = error instanceof Error ? error.message : String(error);
    dependencies.addToast(`New terminal failed: ${message}`, "err");
  }
}
