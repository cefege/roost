import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  AgentStatus,
  asChannelId,
  asSessionId,
  asWorkerFp,
  type AgentStatus as AgentStatusValue,
  type SessionEvent,
} from "@roost/shared/wire";
import { AgentStatusFrameSchema } from "@roost/shared/proto/sync_pb";
import {
  applyAgentStatusFrame,
  resetAgentStatusProjection,
  subscribeAgentStatus,
} from "../src/store/agent-status.ts";
import { rootStore } from "../src/store/root.ts";
import { foldEventIntoStore } from "../src/store/projector.ts";
import {
  markAgentSeen,
  resetAgentSeenForTest,
  seenAgentRevision,
  startAgentSeenPersistence,
} from "../src/lib/agentSeen.ts";
import {
  deriveAgentStatusLevel,
  foldAgentStatusLevels,
  formatAgentStatusCounts,
} from "../src/lib/agentStatus.ts";
import {
  AgentNotificationScheduler,
  classifyAgentTransition,
  countUnseenAgentStatuses,
  type AgentNotificationDelivery,
} from "../src/lib/agentNotificationCore.ts";
import { claimAgentNotification } from "../src/lib/agentNotificationClaim.ts";
import {
  disableDesktopNotifications,
  enableDesktopNotifications,
  notifyPrefs,
  resetNotifyPrefsForTest,
} from "../src/lib/notifyPrefs.ts";

const SESSION_ID = asSessionId("11111111-1111-4111-8111-111111111111");
const OTHER_ID = asSessionId("22222222-2222-4222-8222-222222222222");
const WORKER = asWorkerFp("aa".repeat(32));

class MemoryStorage implements Storage {
  private readonly values = new Map<string, string>();
  get length(): number { return this.values.size; }
  clear(): void { this.values.clear(); }
  getItem(key: string): string | null { return this.values.get(key) ?? null; }
  key(index: number): string | null { return [...this.values.keys()][index] ?? null; }
  removeItem(key: string): void { this.values.delete(key); }
  setItem(key: string, value: string): void { this.values.set(key, String(value)); }
}

class FakeWindow {
  private readonly listeners = new Map<string, Set<(event: any) => void>>();
  addEventListener(type: string, listener: (event: any) => void): void {
    const listeners = this.listeners.get(type) ?? new Set();
    listeners.add(listener);
    this.listeners.set(type, listeners);
  }
  removeEventListener(type: string, listener: (event: any) => void): void {
    this.listeners.get(type)?.delete(listener);
  }
  emit(type: string, event: any): void {
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

const storage = new MemoryStorage();
const fakeWindow = new FakeWindow();
const originalStorage = Object.getOwnPropertyDescriptor(globalThis, "localStorage");
const originalWindow = Object.getOwnPropertyDescriptor(globalThis, "window");
const originalNavigator = Object.getOwnPropertyDescriptor(globalThis, "navigator");

beforeAll(() => {
  Object.defineProperty(globalThis, "localStorage", { configurable: true, value: storage });
  Object.defineProperty(globalThis, "window", { configurable: true, value: fakeWindow });
  Object.defineProperty(globalThis, "navigator", { configurable: true, value: {} });
});

beforeEach(() => {
  storage.clear();
  resetAgentStatusProjection();
  resetAgentSeenForTest();
  resetNotifyPrefsForTest();
});

afterEach(() => vi.useRealTimers());

afterAll(() => {
  if (originalStorage) Object.defineProperty(globalThis, "localStorage", originalStorage);
  else Reflect.deleteProperty(globalThis, "localStorage");
  if (originalWindow) Object.defineProperty(globalThis, "window", originalWindow);
  else Reflect.deleteProperty(globalThis, "window");
  if (originalNavigator) Object.defineProperty(globalThis, "navigator", originalNavigator);
  else Reflect.deleteProperty(globalThis, "navigator");
});

function status(
  state: AgentStatusValue["state"],
  revision: number,
  completedRevision = 0,
  sessionId = SESSION_ID,
  agentId = "omp",
): AgentStatusValue {
  return AgentStatus.parse({
    session_id: sessionId,
    agent_id: agentId,
    state,
    revision,
    completed_revision: completedRevision,
    updated_at: revision,
    active: true,
  });
}

function frame(value: AgentStatusValue | (Omit<AgentStatusValue, "active"> & { active: false })) {
  return create(AgentStatusFrameSchema, {
    sessionId: value.session_id,
    agentId: value.agent_id,
    state: value.state,
    message: value.message,
    revision: BigInt(value.revision),
    completedRevision: BigInt(value.completed_revision),
    updatedAt: value.updated_at,
    active: value.active,
  });
}

function opened(): Extract<SessionEvent, { kind: "opened" }> {
  return {
    kind: "opened",
    session_id: SESSION_ID,
    worker_fp: WORKER,
    channel: asChannelId(1),
    session_kind: "shell",
    cwd: "/repo",
    ts: 1,
  };
}

describe("SPA agent status projection", () => {
  test("orders Sync frames, retains deletion floors, and clears on session close", () => {
    const changes: Array<{ previous: number | null; next: number | null }> = [];
    const unsubscribe = subscribeAgentStatus((change) => changes.push({
      previous: change.previous?.revision ?? null,
      next: change.next?.revision ?? null,
    }));
    try {
      expect(applyAgentStatusFrame(frame(status("working", 10)))).toBe(true);
      expect(rootStore.agent_status[SESSION_ID]?.revision).toBe(10);
      expect(applyAgentStatusFrame(frame(status("blocked", 10)))).toBe(false);
      expect(applyAgentStatusFrame(frame(status("blocked", 9)))).toBe(false);

      const inactive = { ...status("working", 11), active: false as const };
      expect(applyAgentStatusFrame(frame(inactive))).toBe(true);
      expect(rootStore.agent_status[SESSION_ID]).toBeUndefined();
      expect(applyAgentStatusFrame(frame(status("blocked", 10)))).toBe(false);

      foldEventIntoStore(opened());
      expect(applyAgentStatusFrame(frame(status("working", 12)))).toBe(true);
      foldEventIntoStore({ kind: "closed", session_id: SESSION_ID, exit_code: 0, ts: 2 });
      expect(rootStore.agent_status[SESSION_ID]).toBeUndefined();
      expect(changes).toEqual([
        { previous: null, next: 10 },
        { previous: 10, next: null },
        { previous: null, next: 12 },
        { previous: 12, next: null },
      ]);
    } finally {
      unsubscribe();
    }
  });

  // Solid merges an object into the EXISTING store node in place, so a
  // subscriber handed the live proxy would read the post-update value as
  // `previous` and every transition would look like a self-transition -
  // silently suppressing every notification.
  test("publishes a previous snapshot detached from the store node", () => {
    const seen: Array<{ previous: string | null; next: string | null }> = [];
    const unsubscribe = subscribeAgentStatus((change) => seen.push({
      previous: change.previous?.state ?? null,
      next: change.next?.state ?? null,
    }));
    try {
      applyAgentStatusFrame(frame(status("working", 20)));
      applyAgentStatusFrame(frame(status("blocked", 21)));
      expect(seen).toEqual([
        { previous: null, next: "working" },
        { previous: "working", next: "blocked" },
      ]);
    } finally {
      unsubscribe();
    }
  });
});

describe("agent seen acknowledgements", () => {
  test("is monotonic, persists, and merges cross-tab storage maxima", () => {
    const stop = startAgentSeenPersistence();
    try {
      expect(markAgentSeen(SESSION_ID, 7)).toBe(true);
      expect(markAgentSeen(SESSION_ID, 6)).toBe(false);
      fakeWindow.emit("pagehide", {});
      expect(JSON.parse(storage.getItem("roost.agentSeen.v1")!)[SESSION_ID]).toBe(7);

      fakeWindow.emit("storage", {
        key: "roost.agentSeen.v1",
        newValue: JSON.stringify({ [SESSION_ID]: 9, [OTHER_ID]: 4 }),
      });
      expect(seenAgentRevision(SESSION_ID)).toBe(9);
      expect(seenAgentRevision(OTHER_ID)).toBe(4);
    } finally {
      stop();
    }
  });
});

describe("derived status and folder rollups", () => {
  test("uses done only for unseen completions and applies max-priority counts", () => {
    const completed = status("idle", 5, 5);
    expect(deriveAgentStatusLevel(completed, 4)).toBe("done");
    expect(deriveAgentStatusLevel(completed, 5)).toBe("idle");
    expect(deriveAgentStatusLevel(status("blocked", 6), 6)).toBe("blocked");

    const rollup = foldAgentStatusLevels(["idle", "working", "done", "blocked", "working"]);
    expect(rollup.level).toBe("blocked");
    expect(rollup.counts).toMatchObject({ blocked: 1, working: 2, done: 1, idle: 1 });
    expect(formatAgentStatusCounts(rollup.counts)).toBe("1 needs input · 2 working · 1 done · 1 idle");
  });
});

describe("notification transitions", () => {
  test("ignores reconnect baselines and classifies blocked and done boundaries", () => {
    expect(classifyAgentTransition(null, status("blocked", 2))).toBeNull();
    expect(classifyAgentTransition(status("working", 1), status("blocked", 2))).toBe("blocked");
    expect(classifyAgentTransition(status("blocked", 2), status("idle", 3, 3))).toBe("done");
    expect(classifyAgentTransition(status("idle", 3, 3), status("working", 4))).toBeNull();
    expect(classifyAgentTransition(status("working", 4, 3, SESSION_ID, "omp"), status("blocked", 5, 3, SESSION_ID, "pi"))).toBeNull();
  });

  test("cancels replaced timers and suppresses delivery when the session becomes active", () => {
    vi.useFakeTimers();
    const current = new Map<string, AgentStatusValue>();
    const deliveries: AgentNotificationDelivery[] = [];
    const seen: number[] = [];
    let viewed = false;
    const scheduler = new AgentNotificationScheduler({
      statusFor: (sessionId) => current.get(sessionId),
      isViewed: () => viewed,
      markSeen: (_sessionId, revision) => { seen.push(revision); },
      deliver: (delivery) => { deliveries.push(delivery); },
    });

    const working = status("working", 1);
    const blocked = status("blocked", 2);
    current.set(SESSION_ID, blocked);
    scheduler.handle({ sessionId: SESSION_ID, previous: working, next: blocked, revision: 2 });
    expect(scheduler.pendingCount()).toBe(1);

    const resumed = status("working", 3);
    current.set(SESSION_ID, resumed);
    scheduler.handle({ sessionId: SESSION_ID, previous: blocked, next: resumed, revision: 3 });
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toHaveLength(0);

    const blockedAgain = status("blocked", 4);
    current.set(SESSION_ID, blockedAgain);
    scheduler.handle({ sessionId: SESSION_ID, previous: resumed, next: blockedAgain, revision: 4 });
    viewed = true;
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toHaveLength(0);
    expect(seen).toEqual([4]);

    viewed = false;
    const done = status("idle", 5, 5);
    current.set(SESSION_ID, done);
    scheduler.handle({ sessionId: SESSION_ID, previous: blockedAgain, next: done, revision: 5 });
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toEqual([{ sessionId: SESSION_ID, revision: 5, kind: "done" }]);
    scheduler.dispose();
  });

  test("counts only unseen blocked and completion revisions for the title badge", () => {
    const values = [
      status("blocked", 5, 0, SESSION_ID),
      status("idle", 9, 9, OTHER_ID),
      status("working", 12, 9, asSessionId("33333333-3333-4333-8333-333333333333")),
    ];
    expect(countUnseenAgentStatuses(values, () => 0)).toBe(2);
    expect(countUnseenAgentStatuses(values, (id) => id === SESSION_ID ? 5 : 8)).toBe(1);
  });
});

describe("browser-profile delivery preferences", () => {
  test("elects one fallback toast claimant", async () => {
    const results = await Promise.all([
      claimAgentNotification(SESSION_ID, 42, "blocked"),
      claimAgentNotification(SESSION_ID, 42, "blocked"),
    ]);
    expect(results.filter(Boolean)).toHaveLength(1);
    expect(await claimAgentNotification(SESSION_ID, 42, "blocked")).toBe(false);
  });

  test("persists desktop enable only after subscription succeeds", async () => {
    await expect(enableDesktopNotifications(async () => {
      throw new Error("permission denied");
    })).rejects.toThrow("permission denied");
    expect(notifyPrefs().desktop).toBe(false);

    await enableDesktopNotifications(async () => {});
    expect(notifyPrefs().desktop).toBe(true);
    let unsubscribed = false;
    await disableDesktopNotifications(async () => { unsubscribed = true; });
    expect(unsubscribed).toBe(true);
    expect(notifyPrefs().desktop).toBe(false);
  });
});
