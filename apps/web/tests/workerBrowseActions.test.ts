// Worker-browse launches belong to the dashboard generation that started them.
// Deferred spawn and projection promises make each cutover boundary deterministic,
// proving stale continuations cannot publish recents, agent input, navigation, or errors.

import { describe, expect, test } from "bun:test";
import type { Session, WorkerFp } from "@roost/shared/wire";
import {
  launchWorkerBrowseTerminal,
  type _WorkerBrowseLaunchDependencies,
} from "../src/components/workerBrowseActions.ts";

const WORKER_FP = "worker-a" as WorkerFp;
const SESSION = { id: "session-old" } as Session;

interface Deferred<T> {
  readonly promise: Promise<T>;
  readonly resolve: (value: T | PromiseLike<T>) => void;
  readonly reject: (reason?: unknown) => void;
}

interface LaunchHarness {
  readonly spawn: Deferred<string>;
  readonly projection: Deferred<Session | null>;
  readonly projectionStarted: Deferred<void>;
  readonly dependencies: _WorkerBrowseLaunchDependencies;
  readonly cutOverDashboard: () => void;
  readonly waitCalls: () => number;
  readonly recents: string[];
  readonly agentLaunches: string[];
  readonly navigations: string[];
  readonly toasts: Array<{ message: string; kind: string }>;
}

function createLaunchHarness() {
  const spawn = Promise.withResolvers<string>();
  const projection = Promise.withResolvers<Session | null>();
  const projectionStarted = Promise.withResolvers<void>();
  let generation = 1;
  let dashboardId = "dashboard-a";
  let waitCalls = 0;
  const recents: string[] = [];
  const agentLaunches: string[] = [];
  const navigations: string[] = [];
  const toasts: Array<{ message: string; kind: string }> = [];

  const dependencies: _WorkerBrowseLaunchDependencies = {
    captureDashboardResourceToken: () => ({ generation, dashboardId }),
    isCurrentDashboardResourceToken: (token) =>
      token.generation === generation && token.dashboardId === dashboardId,
    spawnShell: () => spawn.promise,
    waitForSession: () => {
      waitCalls += 1;
      projectionStarted.resolve();
      return projection.promise;
    },
    pushRecent: (sessionId) => { recents.push(sessionId); },
    maybeAutoLaunchAgent: (sessionId) => { agentLaunches.push(sessionId); },
    terminalHref: () => "/t/worker-a/old-folder",
    sessionHref: (sessionId) => `/s/${sessionId}`,
    addToast: (message, kind) => {
      toasts.push({ message, kind: kind ?? "ok" });
      return () => undefined;
    },
  };

  return {
    spawn,
    projection,
    projectionStarted,
    dependencies,
    cutOverDashboard: () => {
      generation += 1;
      dashboardId = "dashboard-b";
    },
    waitCalls: () => waitCalls,
    recents,
    agentLaunches,
    navigations,
    toasts,
  };
}

function startLaunch(harness: LaunchHarness): Promise<void> {
  return launchWorkerBrowseTerminal(
    WORKER_FP,
    "/old/folder",
    (href) => { harness.navigations.push(href); },
    harness.dependencies,
  );
}

describe("worker browse dashboard fencing", () => {
  test("a cutover while spawn is pending stops before projection or UI mutation", async () => {
    const harness = createLaunchHarness();
    const launch = startLaunch(harness);

    harness.cutOverDashboard();
    harness.spawn.resolve(SESSION.id);
    await launch;

    expect(harness.waitCalls()).toBe(0);
    expect(harness.recents).toEqual([]);
    expect(harness.agentLaunches).toEqual([]);
    expect(harness.navigations).toEqual([]);
    expect(harness.toasts).toEqual([]);
  });

  test("a cutover during projection wait cannot record or open the old session", async () => {
    const harness = createLaunchHarness();
    const launch = startLaunch(harness);
    harness.spawn.resolve(SESSION.id);
    await harness.projectionStarted.promise;

    harness.cutOverDashboard();
    harness.projection.resolve(SESSION);
    await launch;

    expect(harness.waitCalls()).toBe(1);
    expect(harness.recents).toEqual([]);
    expect(harness.agentLaunches).toEqual([]);
    expect(harness.navigations).toEqual([]);
    expect(harness.toasts).toEqual([]);
  });

  test("a stale rejection does not recreate an old-dashboard toast", async () => {
    const harness = createLaunchHarness();
    const launch = startLaunch(harness);

    harness.cutOverDashboard();
    harness.spawn.reject(new Error("old dashboard failed"));
    await launch;

    expect(harness.toasts).toEqual([]);
    expect(harness.navigations).toEqual([]);
  });

  test("a current-generation failure still reports the spawn error", async () => {
    const harness = createLaunchHarness();
    const launch = startLaunch(harness);
    harness.spawn.reject(new Error("worker offline"));
    await launch;

    expect(harness.toasts).toEqual([{
      message: "New terminal failed: worker offline",
      kind: "err",
    }]);
    expect(harness.navigations).toEqual([]);
  });

  test("the current generation records, launches, and navigates after projection", async () => {
    const harness = createLaunchHarness();
    const launch = startLaunch(harness);
    harness.spawn.resolve(SESSION.id);
    await harness.projectionStarted.promise;
    harness.projection.resolve(SESSION);
    await launch;

    expect(harness.recents).toEqual([SESSION.id]);
    expect(harness.agentLaunches).toEqual([SESSION.id]);
    expect(harness.navigations).toEqual(["/t/worker-a/old-folder"]);
    expect(harness.toasts).toEqual([]);
  });
});
