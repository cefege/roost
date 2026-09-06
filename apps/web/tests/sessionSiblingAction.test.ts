// Sibling-session action coverage for the shared menu/palette launch owner.
// Deferred spawn and projection work proves every continuation is fenced by
// the dashboard token captured before the coordinator call.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { Navigator } from "@solidjs/router";
import type { Session, WorkerFp } from "@roost/shared/wire";
import { spawnSessionSibling } from "../src/lib/sessionSiblingAction.ts";

let dashboardGeneration = 1;
const spawnShell = mock(async (
  _workerFingerprint: string,
  _workingDirectory: string,
) => "new-session");
const waitForSession = mock(async (sessionId: string) => ({ id: sessionId }));
const maybeAutoLaunchAgent = mock((_sessionId: string) => {});
const captureDashboardResourceToken = mock(() => ({
  generation: dashboardGeneration,
  dashboardId: "dashboard-a",
}));
const isCurrentDashboardResourceToken = mock(
  (token: { generation: number }) => token.generation === dashboardGeneration,
);
const recordDiagnostic = mock((_event: string, _fields: Record<string, unknown>) => {});

const dependencies = {
  spawnShell,
  waitForSession,
  maybeAutoLaunchAgent,
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
  recordDiagnostic,
} as unknown as NonNullable<Parameters<typeof spawnSessionSibling>[2]>;

const session: Pick<Session, "worker_fp" | "cwd"> = {
  worker_fp: "worker-a" as WorkerFp,
  cwd: "/work",
};

beforeEach(() => {
  dashboardGeneration = 1;
  spawnShell.mockReset();
  spawnShell.mockImplementation(async () => "new-session");
  waitForSession.mockReset();
  waitForSession.mockImplementation(async (sessionId) => ({ id: sessionId }));
  maybeAutoLaunchAgent.mockClear();
  captureDashboardResourceToken.mockClear();
  isCurrentDashboardResourceToken.mockClear();
  recordDiagnostic.mockClear();
});

describe("sibling-session dashboard fence", () => {
  test("launches and navigates while its dashboard token is current", async () => {
    const navigate = mock((_href: string, _options?: unknown) => {});

    await spawnSessionSibling(session, navigate as unknown as Navigator, dependencies);

    expect(spawnShell).toHaveBeenCalledWith("worker-a", "/work");
    expect(waitForSession).toHaveBeenCalledWith("new-session");
    expect(maybeAutoLaunchAgent).toHaveBeenCalledWith("new-session");
    expect(navigate).toHaveBeenCalledWith("/s/new-session", { replace: false });
  });

  test("a switch while spawn is pending prevents projection and launch", async () => {
    const spawned = Promise.withResolvers<string>();
    spawnShell.mockImplementation(() => spawned.promise);
    const navigate = mock((_href: string, _options?: unknown) => {});

    const pending = spawnSessionSibling(session, navigate as unknown as Navigator, dependencies);
    dashboardGeneration++;
    spawned.resolve("old-dashboard-session");
    await pending;

    expect(waitForSession).not.toHaveBeenCalled();
    expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a switch while projection is pending prevents launch and navigation", async () => {
    const waitStarted = Promise.withResolvers<void>();
    const projected = Promise.withResolvers<{ id: string }>();
    waitForSession.mockImplementation(() => {
      waitStarted.resolve();
      return projected.promise;
    });
    const navigate = mock((_href: string, _options?: unknown) => {});

    const pending = spawnSessionSibling(session, navigate as unknown as Navigator, dependencies);
    await waitStarted.promise;
    dashboardGeneration++;
    projected.resolve({ id: "old-dashboard-session" });
    await pending;

    expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a stale rejection is not reported after the dashboard changes", async () => {
    const spawned = Promise.withResolvers<string>();
    spawnShell.mockImplementation(() => spawned.promise);
    const navigate = mock((_href: string, _options?: unknown) => {});
    const pending = spawnSessionSibling(session, navigate as unknown as Navigator, dependencies);
    dashboardGeneration++;
    spawned.reject(new Error("old dashboard spawn failed"));
    await pending;

    expect(recordDiagnostic).not.toHaveBeenCalled();
    expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a current-dashboard rejection still reports the spawn failure", async () => {
    spawnShell.mockImplementation(async () => {
      throw new Error("spawn denied");
    });
    const navigate = mock((_href: string, _options?: unknown) => {});
    await spawnSessionSibling(session, navigate as unknown as Navigator, dependencies);

    expect(recordDiagnostic).toHaveBeenCalledWith(
      "session.sibling_spawn_failed",
      { error: "spawn denied" },
    );
    expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });
});
