// Dashboard-fenced sibling terminal launch shared by terminal menus and the
// command palette. The existing shell spawn, projection wait, agent launch,
// and navigation sequence remains guarded at every asynchronous boundary.

import type { Navigator } from "@solidjs/router";
import { diag } from "@roost/shared/diag";
import type { Session } from "@roost/shared/wire";
import { spawnShell, waitForSession, maybeAutoLaunchAgent } from "./spawnSession.ts";
import {
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
} from "../store/dashboard-selection.ts";

interface SessionSiblingActionDeps {
  spawnShell: typeof spawnShell;
  waitForSession: typeof waitForSession;
  maybeAutoLaunchAgent: typeof maybeAutoLaunchAgent;
  captureDashboardResourceToken: typeof captureDashboardResourceToken;
  isCurrentDashboardResourceToken: typeof isCurrentDashboardResourceToken;
  recordDiagnostic: typeof diag;
}

const defaultSessionSiblingActionDeps: SessionSiblingActionDeps = {
  spawnShell,
  waitForSession,
  maybeAutoLaunchAgent,
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
  recordDiagnostic: diag,
};

export async function spawnSessionSibling(
  session: Pick<Session, "worker_fp" | "cwd">,
  navigate: Navigator,
  deps: SessionSiblingActionDeps = defaultSessionSiblingActionDeps,
): Promise<void> {
  const dashboardToken = deps.captureDashboardResourceToken();
  try {
    const sessionId = await deps.spawnShell(session.worker_fp, session.cwd);
    if (!deps.isCurrentDashboardResourceToken(dashboardToken)) return;
    const projectedSession = await deps.waitForSession(sessionId);
    if (!deps.isCurrentDashboardResourceToken(dashboardToken)) return;
    deps.maybeAutoLaunchAgent(sessionId);
    if (!deps.isCurrentDashboardResourceToken(dashboardToken)) return;
    if (projectedSession) navigate(`/s/${projectedSession.id}`, { replace: false });
  } catch (error) {
    if (!deps.isCurrentDashboardResourceToken(dashboardToken)) return;
    deps.recordDiagnostic("session.sibling_spawn_failed", {
      error: error instanceof Error ? error.message : String(error),
    });
  }
}
