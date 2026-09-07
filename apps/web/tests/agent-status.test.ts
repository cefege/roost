// Browser agent-status tests cover legacy display, occupant-pinned seen state,
// notification scheduling, and presentation rollups. Released-occupant
// retirement lives in the sibling agent-status-exit.test.ts; the fake profile
// storage both files share lives in ./agent-status-test-harness.ts.

import { afterAll, afterEach, beforeAll, beforeEach, describe, expect, test, vi } from "bun:test";
import {
  AgentStatus,
  asChannelId,
  asSessionId,
  asWorkerFp,
  type AgentStatus as AgentStatusValue,
  type AgentStatusIdentity,
  type SessionEvent,
} from "@roost/shared/wire";
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
  agentStatusRevisionToken,
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
import {
  IDENTITY_A,
  IDENTITY_B,
  OTHER_ID,
  SESSION_ID,
  fakeWindow,
  frame,
  installBrowserProfileGlobals,
  status,
  storage,
} from "./agent-status-test-harness.ts";

const WORKER = asWorkerFp("aa".repeat(32));
let restoreBrowserProfileGlobals = () => {};

beforeAll(() => { restoreBrowserProfileGlobals = installBrowserProfileGlobals(); });

beforeEach(() => {
  storage.clear();
  resetAgentStatusProjection();
  resetAgentSeenForTest();
});

afterEach(() => vi.useRealTimers());

afterAll(() => { restoreBrowserProfileGlobals(); });

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

  test("publishes a previous snapshot detached from the store node", () => {
    let previous: AgentStatusValue | null = null;
    const unsubscribe = subscribeAgentStatus((change) => { previous = change.previous; });
    try {
      applyAgentStatusFrame(frame(status("working", 20)));
      applyAgentStatusFrame(frame(status("blocked", 21)));
      expect(previous).toMatchObject({ state: "working" });
    } finally {
      unsubscribe();
    }
  });
});

describe("agent seen acknowledgements", () => {
  test("persists v2 tokens and merges exact identities across tabs", () => {
    const legacy = status("idle", 7, 7);
    const occupantA = status("idle", 9, 9, SESSION_ID, "omp", IDENTITY_A);
    const occupantB = status("idle", 4, 4, SESSION_ID, "omp", IDENTITY_B);
    const identityAScreen: AgentStatusIdentity = { ...IDENTITY_A, source: "screen" };
    const lowerOccupantA = status("idle", 8, 8, SESSION_ID, "omp", identityAScreen);
    const higherOccupantA = status("idle", 10, 10, SESSION_ID, "omp", identityAScreen);
    const legacyOnly = status("idle", 12, 12, OTHER_ID);
    const freshIdentified = status("blocked", 0, 0, OTHER_ID, "omp", IDENTITY_A);
    storage.setItem("roost.agentSeen.v1", JSON.stringify({ [SESSION_ID]: 6 }));
    const stop = startAgentSeenPersistence();
    try {
      expect(seenAgentRevision(legacy)).toBe(6);
      expect(markAgentSeen(legacy)).toBe(true);
      expect(markAgentSeen(status("idle", 6, 6))).toBe(false);
      fakeWindow.emit("pagehide", {});
      const stored = JSON.parse(storage.getItem("roost.agentSeen.v2")!);
      expect(stored).toMatchObject({
        schema_version: 2,
        tokens: [{ session_id: SESSION_ID, revision: 7 }],
      });
      expect(storage.getItem("roost.agentSeen.v1")).toBeNull();

      const storedA = JSON.stringify({
        schema_version: 2,
        tokens: [agentStatusRevisionToken(occupantA)],
      });
      storage.setItem("roost.agentSeen.v2", storedA);
      fakeWindow.emit("storage", { key: "roost.agentSeen.v2", newValue: storedA });
      const storedB = JSON.stringify({
        schema_version: 2,
        tokens: [agentStatusRevisionToken(occupantB)],
      });
      storage.setItem("roost.agentSeen.v2", storedB);
      fakeWindow.emit("storage", { key: "roost.agentSeen.v2", newValue: storedB });
      fakeWindow.emit("pagehide", {});
      const persistedTokens = JSON.parse(storage.getItem("roost.agentSeen.v2")!).tokens;
      expect(persistedTokens).toEqual(expect.arrayContaining([
        agentStatusRevisionToken(occupantA),
        agentStatusRevisionToken(occupantB),
      ]));
      fakeWindow.emit("storage", {
        key: "roost.agentSeen.v2",
        newValue: JSON.stringify({
          schema_version: 2,
          tokens: [
            agentStatusRevisionToken(lowerOccupantA),
            agentStatusRevisionToken(higherOccupantA),
          ],
        }),
      });
      expect(seenAgentRevision(occupantA)).toBe(10);
      expect(seenAgentRevision(AgentStatus.parse({ ...occupantA, source: "screen" }))).toBe(10);
      expect(seenAgentRevision(occupantB)).toBe(4);

      fakeWindow.emit("storage", {
        key: "roost.agentSeen.v1",
        newValue: JSON.stringify({ [SESSION_ID]: 12, [OTHER_ID]: 12 }),
      });
      expect(seenAgentRevision(legacy)).toBe(12);
      expect(seenAgentRevision(legacyOnly)).toBe(12);
      expect(seenAgentRevision(freshIdentified)).toBe(-1);
      expect(seenAgentRevision(occupantA)).toBe(10);
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

    const acknowledged = status("idle", 9, 9, SESSION_ID, "omp", IDENTITY_A);
    const revisionZero = status("blocked", 0, 0, SESSION_ID, "omp", IDENTITY_B);
    markAgentSeen(acknowledged);
    expect(deriveAgentStatusLevel(acknowledged, seenAgentRevision(acknowledged))).toBe("idle");
    expect(seenAgentRevision(revisionZero)).toBe(-1);
    expect(countUnseenAgentStatuses([revisionZero], seenAgentRevision)).toBe(1);
    expect(markAgentSeen(revisionZero)).toBe(true);
    const replacement = status("idle", 1, 1, SESSION_ID, "omp", IDENTITY_B);
    expect(deriveAgentStatusLevel(replacement, seenAgentRevision(replacement))).toBe("done");
    expect(countUnseenAgentStatuses([replacement], seenAgentRevision)).toBe(1);

    const rollup = foldAgentStatusLevels(["idle", "working", "done", "blocked", "working"]);
    expect(rollup.level).toBe("blocked");
    expect(rollup.counts).toMatchObject({ blocked: 1, working: 2, done: 1, idle: 1 });
    expect(formatAgentStatusCounts(rollup.counts)).toBe("1 needs input · 2 working · 1 done · 1 idle");
  });
});

describe("notification transitions", () => {
  test("ignores baselines and cross-occupant transitions", () => {
    expect(classifyAgentTransition(null, status("blocked", 2))).toBeNull();
    expect(classifyAgentTransition(status("working", 1), status("blocked", 2))).toBe("blocked");
    const screenBlocked = AgentStatus.parse({
      ...status("blocked", 2, 0, SESSION_ID, "omp", IDENTITY_A),
      source: "screen",
    });
    expect(classifyAgentTransition(
      status("working", 1, 0, SESSION_ID, "omp", IDENTITY_A),
      screenBlocked,
    )).toBe("blocked");
    expect(classifyAgentTransition(status("blocked", 2), status("idle", 3, 3))).toBe("done");
    expect(classifyAgentTransition(
      status("working", 4, 3, SESSION_ID, "omp", IDENTITY_A),
      status("blocked", 1, 0, SESSION_ID, "omp", IDENTITY_B),
    )).toBeNull();
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
      markSeen: (seenStatus) => { seen.push(seenStatus.revision); },
      deliver: (delivery) => { deliveries.push(delivery); },
    });

    const working = status("working", 1, 0, SESSION_ID, "omp", IDENTITY_A);
    const blocked = status("blocked", 2, 0, SESSION_ID, "omp", IDENTITY_A);
    current.set(SESSION_ID, blocked);
    scheduler.handle({ sessionId: SESSION_ID, previous: working, next: blocked, revision: 2 });
    expect(scheduler.pendingCount()).toBe(1);

    const blockedReplacement = status("blocked", 2, 0, SESSION_ID, "omp", IDENTITY_B);
    current.set(SESSION_ID, blockedReplacement);
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toHaveLength(0);

    const resumed = status("working", 3, 0, SESSION_ID, "omp", IDENTITY_B);
    current.set(SESSION_ID, resumed);
    scheduler.handle({
      sessionId: SESSION_ID,
      previous: blockedReplacement,
      next: resumed,
      revision: 3,
    });
    const blockedAgain = status("blocked", 4, 0, SESSION_ID, "omp", IDENTITY_B);
    current.set(SESSION_ID, blockedAgain);
    scheduler.handle({ sessionId: SESSION_ID, previous: resumed, next: blockedAgain, revision: 4 });
    viewed = true;
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toHaveLength(0);
    expect(seen).toEqual([4]);

    viewed = false;
    const done = status("idle", 5, 5, SESSION_ID, "omp", IDENTITY_B);
    current.set(SESSION_ID, done);
    scheduler.handle({ sessionId: SESSION_ID, previous: blockedAgain, next: done, revision: 5 });
    vi.advanceTimersByTime(1_000);
    expect(deliveries).toEqual([{
      sessionId: SESSION_ID,
      token: agentStatusRevisionToken(done),
      statusRevision: 5,
      kind: "done",
      completedRevision: 5,
    }]);
    scheduler.dispose();
  });

  test("counts only unseen blocked and completion revisions for the title badge", () => {
    const values = [
      status("blocked", 5, 0, SESSION_ID),
      status("idle", 9, 9, OTHER_ID),
      status("working", 12, 9, asSessionId("33333333-3333-4333-8333-333333333333")),
    ];
    expect(countUnseenAgentStatuses(values, () => 0)).toBe(2);
    expect(countUnseenAgentStatuses(
      values,
      (candidate) => candidate.session_id === SESSION_ID ? 5 : 8,
    )).toBe(1);
  });
});

