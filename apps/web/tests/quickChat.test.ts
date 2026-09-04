// Quick-chat path classification and dashboard-scoped spawn orchestration.
// Deferred RPC and projection promises prove that a dashboard cutover drops
// every old-scope continuation before it can spawn, navigate, launch, or toast.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import type { Navigator } from "@solidjs/router";

const workerFp = "worker-a";
let dashboardGeneration = 1;

const filesMkdir = mock(async (_request: { workerFp: string; path: string }) => ({
  resolvedPath: "/resolved/chat",
}));
const spawnShell = mock(async (
  _fp: string,
  _cwd: string,
  sessionId?: string,
) => sessionId ?? "spawned-session");
const waitForSession = mock(async (_sessionId: string) => null);
const forceLaunchAgent = mock((_sessionId: string) => {});
const addToast = mock((_message: string, _kind: string) => {});
const captureDashboardResourceToken = mock(() => ({
  generation: dashboardGeneration,
  dashboardId: "dashboard-a",
}));
const isCurrentDashboardResourceToken = mock(
  (token: { generation: number }) => token.generation === dashboardGeneration,
);

mock.module("../src/connect.ts", () => ({
  coordClient: { filesMkdir },
}));
mock.module("../src/store/root.ts", () => ({
  rootStore: { workers: { [workerFp]: { fp: workerFp } } },
}));
mock.module("../src/store/selectors.ts", () => ({
  allSessions: () => [],
}));
mock.module("../src/store/sync.ts", () => ({
  workerOnline: () => true,
}));
mock.module("../src/lib/spawnSession.ts", () => ({
  spawnShell,
  waitForSession,
  forceLaunchAgent,
}));
mock.module("../src/store/toastStore.ts", () => ({
  addToast,
}));
mock.module("../src/store/dashboard-selection.ts", () => ({
  captureDashboardResourceToken,
  isCurrentDashboardResourceToken,
}));

// Module mocks must install before quickChat binds its singleton dependencies.
const {
  isChatFolder,
  newChatFolderPath,
  startQuickChat,
} = await import("../src/lib/quickChat.ts");

beforeEach(() => {
  dashboardGeneration = 1;
  filesMkdir.mockReset();
  filesMkdir.mockImplementation(async () => ({ resolvedPath: "/resolved/chat" }));
  spawnShell.mockReset();
  spawnShell.mockImplementation(async (_fp, _cwd, sessionId) => sessionId ?? "spawned-session");
  waitForSession.mockReset();
  waitForSession.mockImplementation(async () => null);
  forceLaunchAgent.mockClear();
  addToast.mockClear();
  captureDashboardResourceToken.mockClear();
  isCurrentDashboardResourceToken.mockClear();
});

describe("isChatFolder", () => {
  test("true for a chat scratch dir", () => {
    expect(isChatFolder("/Users/x/.roost/chats/chat-20260724-120000-ab12")).toBe(true);
  });

  test("false for a real workspace", () => {
    expect(isChatFolder("/Users/x/Code/idea")).toBe(false);
  });
});

describe("newChatFolderPath", () => {
  test("matches the expected shape", () => {
    expect(newChatFolderPath()).toMatch(/^~\/\.roost\/chats\/chat-\d{8}-\d{6}-.{4}$/);
  });

  test("two consecutive calls differ", () => {
    expect(newChatFolderPath()).not.toBe(newChatFolderPath());
  });
});

describe("startQuickChat dashboard fence", () => {
  test("runs the complete spawn flow while its dashboard token is current", async () => {
    const navigate = mock((_href: string) => {});

    await startQuickChat(navigate as unknown as Navigator);

    expect(filesMkdir).toHaveBeenCalledTimes(1);
    expect(spawnShell).toHaveBeenCalledTimes(1);
    const sessionId = spawnShell.mock.calls[0][2];
    expect(spawnShell.mock.calls[0].slice(0, 2)).toEqual([workerFp, "/resolved/chat"]);
    expect(waitForSession).toHaveBeenCalledWith(sessionId);
    expect(navigate).toHaveBeenCalledWith(`/s/${sessionId}`);
    expect(forceLaunchAgent).toHaveBeenCalledWith(sessionId);
    expect(addToast).not.toHaveBeenCalled();
  });

  test("a switch while mkdir is pending prevents the spawn continuation", async () => {
    const mkdir = Promise.withResolvers<{ resolvedPath: string }>();
    filesMkdir.mockImplementation(() => mkdir.promise);
    const navigate = mock((_href: string) => {});

    const pending = startQuickChat(navigate as unknown as Navigator);
    dashboardGeneration++;
    mkdir.resolve({ resolvedPath: "/old-dashboard/chat" });
    await pending;

    expect(spawnShell).not.toHaveBeenCalled();
    expect(waitForSession).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(forceLaunchAgent).not.toHaveBeenCalled();
    expect(addToast).not.toHaveBeenCalled();
  });

  test("a switch while spawn is pending prevents projection and launch", async () => {
    const spawnStarted = Promise.withResolvers<void>();
    const spawned = Promise.withResolvers<string>();
    spawnShell.mockImplementation(() => {
      spawnStarted.resolve();
      return spawned.promise;
    });
    const navigate = mock((_href: string) => {});

    const pending = startQuickChat(navigate as unknown as Navigator);
    await spawnStarted.promise;
    dashboardGeneration++;
    spawned.resolve("old-dashboard-session");
    await pending;

    expect(waitForSession).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(forceLaunchAgent).not.toHaveBeenCalled();
    expect(addToast).not.toHaveBeenCalled();
  });

  test("a switch while projection is pending prevents navigation and input", async () => {
    const waitStarted = Promise.withResolvers<void>();
    const projected = Promise.withResolvers<null>();
    waitForSession.mockImplementation(() => {
      waitStarted.resolve();
      return projected.promise;
    });
    const navigate = mock((_href: string) => {});

    const pending = startQuickChat(navigate as unknown as Navigator);
    await waitStarted.promise;
    dashboardGeneration++;
    projected.resolve(null);
    await pending;

    expect(navigate).not.toHaveBeenCalled();
    expect(forceLaunchAgent).not.toHaveBeenCalled();
    expect(addToast).not.toHaveBeenCalled();
  });

  test("a stale rejection does not recreate an old-dashboard error toast", async () => {
    const mkdir = Promise.withResolvers<{ resolvedPath: string }>();
    filesMkdir.mockImplementation(() => mkdir.promise);
    const navigate = mock((_href: string) => {});

    const pending = startQuickChat(navigate as unknown as Navigator);
    dashboardGeneration++;
    mkdir.reject(new Error("old dashboard rejected mkdir"));
    await pending;

    expect(addToast).not.toHaveBeenCalled();
    expect(navigate).not.toHaveBeenCalled();
    expect(forceLaunchAgent).not.toHaveBeenCalled();
  });

  test("a current-dashboard rejection still reports the spawn failure", async () => {
    filesMkdir.mockImplementation(async () => {
      throw new Error("mkdir denied");
    });
    const navigate = mock((_href: string) => {});

    await startQuickChat(navigate as unknown as Navigator);

    expect(addToast).toHaveBeenCalledWith("New chat failed: mkdir denied", "err");
    expect(navigate).not.toHaveBeenCalled();
    expect(forceLaunchAgent).not.toHaveBeenCalled();
  });
});
