// Terminal context-menu spawn and mobile sheet regression coverage.
// Deferred work proves dashboard cutovers stop old-scope continuations, while
// style assertions pin the keyboard and safe-area viewport constraints.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { Navigator } from "@solidjs/router";
import type { Session, WorkerFp } from "@roost/shared/wire";

const jsxFragment = Symbol("terminal-context-test-fragment");
mock.module("react/jsx-dev-runtime", () => ({
  Fragment: jsxFragment,
  jsxDEV: (tag: unknown, props: Record<string, unknown> | null) => ({ tag, props }),
}));
mock.module("@solidjs/router", () => ({
  useNavigate: () => () => {},
  useLocation: () => ({ pathname: "/" }),
}));

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

mock.module("../src/lib/spawnSession.ts", () => ({
  spawnShell,
  waitForSession,
  maybeAutoLaunchAgent,
}));
mock.module("../src/store/dashboard-selection.ts", () => ({
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
}));

// These dependency mocks must install before the TSX module binds its imports.
const {
  _spawnContextTerminal,
  _terminalActionSheetStyle,
} = await import("../src/components/TerminalContextMenu.tsx");

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
});

describe("context-menu dashboard fence", () => {
  test("launches and navigates while its dashboard token is current", async () => {
    const navigate = mock((_href: string, _options?: unknown) => {});

    await _spawnContextTerminal(session, navigate as unknown as Navigator);

    expect(spawnShell).toHaveBeenCalledWith("worker-a", "/work");
    expect(waitForSession).toHaveBeenCalledWith("new-session");
    expect(maybeAutoLaunchAgent).toHaveBeenCalledWith("new-session");
    expect(navigate).toHaveBeenCalledWith("/s/new-session", { replace: false });
  });

  test("a switch while spawn is pending prevents projection and launch", async () => {
    const spawned = Promise.withResolvers<string>();
    spawnShell.mockImplementation(() => spawned.promise);
    const navigate = mock((_href: string, _options?: unknown) => {});

    const pending = _spawnContextTerminal(session, navigate as unknown as Navigator);
    dashboardGeneration++;
    spawned.resolve("old-dashboard-session");
    await pending;

    expect(waitForSession).not.toHaveBeenCalled();
    expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
  });

  test("a switch while projection is pending prevents input and navigation", async () => {
    const waitStarted = Promise.withResolvers<void>();
    const projected = Promise.withResolvers<{ id: string }>();
    waitForSession.mockImplementation(() => {
      waitStarted.resolve();
      return projected.promise;
    });
    const navigate = mock((_href: string, _options?: unknown) => {});

    const pending = _spawnContextTerminal(session, navigate as unknown as Navigator);
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
    const warn = mock((..._messages: unknown[]) => {});
    const originalWarn = console.warn;
    console.warn = warn;

    try {
      const pending = _spawnContextTerminal(session, navigate as unknown as Navigator);
      dashboardGeneration++;
      spawned.reject(new Error("old dashboard spawn failed"));
      await pending;

      expect(warn).not.toHaveBeenCalled();
      expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
      expect(navigate).not.toHaveBeenCalled();
    } finally {
      console.warn = originalWarn;
    }
  });

  test("a current-dashboard rejection still reports the spawn failure", async () => {
    spawnShell.mockImplementation(async () => {
      throw new Error("spawn denied");
    });
    const navigate = mock((_href: string, _options?: unknown) => {});
    const warn = mock((..._messages: unknown[]) => {});
    const originalWarn = console.warn;
    console.warn = warn;

    try {
      await _spawnContextTerminal(session, navigate as unknown as Navigator);

      expect(warn).toHaveBeenCalledWith(
        "[ctx] new terminal failed",
        expect.objectContaining({ message: "spawn denied" }),
      );
      expect(maybeAutoLaunchAgent).not.toHaveBeenCalled();
      expect(navigate).not.toHaveBeenCalled();
    } finally {
      console.warn = originalWarn;
    }
  });
});

test("mobile action sheet stays inside the keyboard-safe viewport", () => {
  const style = _terminalActionSheetStyle();

  expect(style["box-sizing"]).toBe("border-box");
  expect(style.bottom).toBe("max(var(--kb-offset), 0px)");
  expect(style["max-height"]).toBe(
    "calc(100dvh - max(var(--kb-offset), 0px) - var(--md-space-4))",
  );
  expect(style["overflow-y"]).toBe("auto");
  expect(String(style.padding)).toContain("env(safe-area-inset-bottom, 0px)");
});
