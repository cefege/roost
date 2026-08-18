// Invariant tests for the web store projection layer: shared-fold determinism,
// visible spawned rows, and agreement with the SPA projector.

import { expect, test, describe } from "bun:test";
import { foldEvent, foldAll, asWorkerFp, asSessionId, asChannelId, asWorkspaceId, SessionEvent as SessionEventSchema } from "@roost/shared/wire";
import type { SessionEvent, Session } from "@roost/shared/wire";
import { eventToProto, protoToEvent } from "@roost/shared/wire/event-proto";
import { applySessionsSnapshot, foldEventIntoStore } from "../src/store/projector.ts";
import { beginOptimisticSpawn, endOptimisticSpawn } from "../src/store/optimisticSpawn.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  _resetTerminalOutboundForTest,
  acquireTerminalViewportOwner,
  terminalOutboundSnapshot,
} from "../src/ws/sync-outbound.ts";

// ──────────────────────────────────────────────────────────────────────
// Helpers to build canonical test events. Use as* constructors for branded types.
// ──────────────────────────────────────────────────────────────────────

const FP = asWorkerFp("a".repeat(64));
const SESSION_ID = asSessionId("00000000-0000-0000-0000-000000000001");
const WS_ID = asWorkspaceId("00000000-0000-0000-0000-000000000002");

function makeOpenedEvent(overrides: Partial<Extract<SessionEvent, { kind: "opened" }>> = {}): Extract<SessionEvent, { kind: "opened" }> {
  return {
    kind: "opened",
    session_id: SESSION_ID,
    worker_fp: FP,
    channel: asChannelId(1),
    session_kind: "shell",
    cwd: "/home/user/project",
    ts: 1000,
    ...overrides,
  };
}


// ──────────────────────────────────────────────────────────────────────
// Test 1: foldEvent determinism — shared foldEvent vs manual fold
// ──────────────────────────────────────────────────────────────────────

describe("foldEvent determinism", () => {
  test("opened event creates session", () => {
    const events: SessionEvent[] = [makeOpenedEvent()];
    const result = foldAll(events);
    const session = result.get(SESSION_ID);
    expect(session).toBeDefined();
    expect(session!.id).toBe(SESSION_ID);
    expect(session!.status).toBe("open");
    expect(session!.worker_fp).toBe(FP);
    expect(session!.cwd).toBe("/home/user/project");
  });


  test("closed event deletes the session (no closed limbo)", () => {
    const events: SessionEvent[] = [
      makeOpenedEvent(),
      { kind: "closed", session_id: SESSION_ID, exit_code: 0, ts: 3000 },
    ];
    const result = foldAll(events);
    expect(result.has(SESSION_ID)).toBe(false);
  });

  test("cwd event updates cwd", () => {
    const events: SessionEvent[] = [
      makeOpenedEvent(),
      { kind: "cwd", session_id: SESSION_ID, cwd: "/new/path", ts: 1500 },
    ];
    const result = foldAll(events);
    expect(result.get(SESSION_ID)!.cwd).toBe("/new/path");
  });

  test("workspace_assigned event sets workspace_id", () => {
    const events: SessionEvent[] = [
      makeOpenedEvent(),
      { kind: "workspace_assigned", session_id: SESSION_ID, workspace_id: WS_ID, ts: 1500 },
    ];
    const result = foldAll(events);
    expect(result.get(SESSION_ID)!.workspace_id).toBe(WS_ID);
  });

  test("replay is deterministic — same result for same event sequence", () => {
    const events: SessionEvent[] = [
      makeOpenedEvent(),
      { kind: "cwd", session_id: SESSION_ID, cwd: "/replayed", ts: 1500 },
    ];
    const r1 = foldAll(events);
    const r2 = foldAll(events);
    expect(r1.get(SESSION_ID)).toEqual(r2.get(SESSION_ID));
  });
});

// ──────────────────────────────────────────────────────────────────────
// Spawn produces visible rows
// ──────────────────────────────────────────────────────────────────────

describe("spawn produces visible row", () => {
  test("opened event immediately yields a session row", () => {
    const sessions = foldAll([makeOpenedEvent()]);
    expect(sessions.size).toBe(1);
    const s = sessions.get(SESSION_ID)!;
    expect(s.id).toBe(SESSION_ID);
    expect(s.status).toBe("open");
  });

  test("multiple spawns yield multiple rows", () => {
    const id2 = asSessionId("00000000-0000-0000-0000-000000000004");
    const events: SessionEvent[] = [
      makeOpenedEvent({ session_id: SESSION_ID }),
      makeOpenedEvent({ session_id: id2, channel: asChannelId(3) }),
    ];
    const sessions = foldAll(events);
    expect(sessions.size).toBe(2);
  });

  test("snapshot installs announced + KEEPS ghosts as breadcrumbs", () => {
    const ghostId = asSessionId("00000000-0000-0000-0000-000000000005");
    // Ghost was open in DB but not in snapshot.
    const prev = foldAll([makeOpenedEvent({ session_id: ghostId })]);
    // Snapshot announces only SESSION_ID.
    const snapshotSession: Session = {
      id: SESSION_ID,
      worker_fp: FP,
      channel: asChannelId(1),
      kind: "shell",
      cwd: "/fresh",
      workspace_id: null,
      status: "open",
      created_at: 2000,
      closed_at: null,
      custom_title: null,
    };
    const result = foldEvent(prev, {
      kind: "snapshot",
      worker_fp: FP,
      sessions: [snapshotSession],
      ts: 3000,
    });
    // Breadcrumb model: the ghost is absent from the snapshot but PERSISTS as an
    // offline row (only an explicit `closed`/✕ removes it) so a worker restart
    // doesn't wipe the sidebar. The announced session is also installed.
    expect(result.has(ghostId)).toBe(true);
    expect(result.has(SESSION_ID)).toBe(true);
    expect(result.get(SESSION_ID)!.cwd).toBe("/fresh");
  });
});

// ──────────────────────────────────────────────────────────────────────
// PROJECTION AGREEMENT — the SPA projector (foldEventIntoStore →
// rootStore) MUST produce the same per-key result as the shared foldAll.
// This is the tripwire the audit (wf_728b67c1) said would have caught the
// `respawned`-dropped drift: the OLD projector was a hand-mirror that
// could (and did) diverge. It now delegates to foldEvent, so this holds
// by construction — and this test fails loudly if anyone re-introduces a
// divergent hand-switch. Drives the REAL global rootStore.
// ──────────────────────────────────────────────────────────────────────

function resetSessions(): void {
  for (const id of Object.keys(rootStore.sessions)) {
    setRootStore("sessions", id, undefined as unknown as Session);
  }
}
function storeSessions(): Map<string, Session> {
  return new Map(Object.entries(rootStore.sessions) as [string, Session][]);
}
// JSON round-trip strips Solid proxies so toEqual compares structure, not identity.
const norm = (m: Map<string, Session>) =>
  Object.fromEntries([...m].map(([k, v]) => [k, JSON.parse(JSON.stringify(v))]));

describe("projection agreement — foldEventIntoStore === shared foldAll", () => {
  test("comprehensive stream (opened/cwd/workspace/respawned/closed) agrees", () => {
    resetSessions();
    const id2 = asSessionId("00000000-0000-0000-0000-000000000006");
    const events: SessionEvent[] = [
      makeOpenedEvent({ session_id: SESSION_ID }),
      makeOpenedEvent({ session_id: id2, channel: asChannelId(7) }),
      { kind: "cwd", session_id: SESSION_ID, cwd: "/moved", ts: 1500 },
      { kind: "workspace_assigned", session_id: SESSION_ID, workspace_id: WS_ID, ts: 1600 },
      { kind: "respawned", session_id: SESSION_ID, new_channel: asChannelId(9), ts: 1700 },
      { kind: "closed", session_id: id2, exit_code: 0, ts: 1800 },
    ];
    for (const e of events) foldEventIntoStore(e);
    expect(norm(storeSessions())).toEqual(norm(foldAll(events)));
  });

  test("respawned is applied (regression: the exact variant the hand-mirror dropped)", () => {
    resetSessions();
    foldEventIntoStore(makeOpenedEvent({ session_id: SESSION_ID, channel: asChannelId(1) }));
    foldEventIntoStore({ kind: "respawned", session_id: SESSION_ID, new_channel: asChannelId(42), ts: 5000 });
    const s = rootStore.sessions[SESSION_ID]!;
    expect(s.channel).toBe(asChannelId(42));
    expect(s.status).toBe("open");
  });

  test("snapshot via the store KEEPS ghosts as breadcrumbs + installs announced", () => {
    resetSessions();
    const ghostId = asSessionId("00000000-0000-0000-0000-000000000005");
    foldEventIntoStore(makeOpenedEvent({ session_id: ghostId }));
    foldEventIntoStore(makeOpenedEvent({ session_id: SESSION_ID, channel: asChannelId(1) }));
    const snap: Session = {
      id: SESSION_ID, worker_fp: FP, channel: asChannelId(1), kind: "shell",
      cwd: "/fresh", workspace_id: null, status: "open",
      created_at: 2000, closed_at: null, custom_title: null,
    };
    foldEventIntoStore({ kind: "snapshot", worker_fp: FP, sessions: [snap], ts: 9000 });
    // Breadcrumb model: the store projector (delegates to shared foldEvent) keeps
    // the un-announced ghost, matching foldAll — projections stay in agreement.
    expect(rootStore.sessions[ghostId]).toBeDefined();
    expect(rootStore.sessions[SESSION_ID]?.cwd).toBe("/fresh");
  });

  test("A7: snapshot preserves coord-owned workspace_id (worker announces null)", () => {
    resetSessions();
    // Session is open and assigned to a workspace.
    foldEventIntoStore(makeOpenedEvent({ session_id: SESSION_ID, channel: asChannelId(1) }));
    foldEventIntoStore({ kind: "workspace_assigned", session_id: SESSION_ID, workspace_id: WS_ID, ts: 1100 });
    expect(rootStore.sessions[SESSION_ID]?.workspace_id).toBe(WS_ID);
    // Worker restarts → re-announces the session in a snapshot with
    // workspace_id:null (it doesn't track that field). Grouping must survive.
    const snap: Session = {
      id: SESSION_ID, worker_fp: FP, channel: asChannelId(2), kind: "shell",
      cwd: "/home/user/project", workspace_id: null, status: "open",
      created_at: 1000, closed_at: null, custom_title: null,
    };
    foldEventIntoStore({ kind: "snapshot", worker_fp: FP, sessions: [snap], ts: 2000 });
    expect(rootStore.sessions[SESSION_ID]?.workspace_id).toBe(WS_ID); // preserved
  });
});

// ──────────────────────────────────────────────────────────────────────
// SNAPSHOT RECONCILIATION — a full session set (bootstrap hydrate, and the
// re-hydration every reconnect's fresh domain generation triggers) must UPDATE
// the per-session record in place. A whole-record `setRootStore("sessions",
// rec)` is a key-by-key Solid merge: it replaced every session object on every
// reconnect (invalidating every subscriber, remounting anything keyed by object
// identity) and pruned nothing, so closed sessions lingered until a reload.
// ──────────────────────────────────────────────────────────────────────

const OTHER_ID = asSessionId("00000000-0000-0000-0000-000000000007");

function makeSession(id: string, overrides: Partial<Session> = {}): Session {
  return {
    id: asSessionId(id),
    worker_fp: FP,
    channel: asChannelId(1),
    kind: "shell",
    cwd: "/home/user/project",
    workspace_id: null,
    status: "open",
    created_at: 1000,
    closed_at: null,
    custom_title: null,
    ...overrides,
  };
}

describe("full session snapshots reconcile per id", () => {
  test("unchanged ids keep their store object; only changed leaves move", () => {
    resetSessions();
    applySessionsSnapshot({
      [SESSION_ID]: makeSession(SESSION_ID),
      [OTHER_ID]: makeSession(OTHER_ID, { channel: asChannelId(7) }),
    });
    const mounted = rootStore.sessions[SESSION_ID]!;
    const untouched = rootStore.sessions[OTHER_ID]!;

    // Reconnect re-hydration: same ids, complete objects, one changed field.
    applySessionsSnapshot({
      [SESSION_ID]: makeSession(SESSION_ID, { cwd: "/moved" }),
      [OTHER_ID]: makeSession(OTHER_ID, { channel: asChannelId(7) }),
    });
    expect(rootStore.sessions[SESSION_ID]).toBe(mounted);
    expect(rootStore.sessions[OTHER_ID]).toBe(untouched);
    expect(rootStore.sessions[SESSION_ID]!.cwd).toBe("/moved");
    expect(rootStore.sessions[OTHER_ID]!.channel).toBe(asChannelId(7));
  });

  test("absent ids are pruned with their volatile slices", () => {
    resetSessions();
    applySessionsSnapshot({
      [SESSION_ID]: makeSession(SESSION_ID),
      [OTHER_ID]: makeSession(OTHER_ID),
    });
    const kept = rootStore.sessions[SESSION_ID]!;
    setRootStore("terminal_title", OTHER_ID, "vim");
    setRootStore("last_activity", OTHER_ID, 42);

    applySessionsSnapshot({ [SESSION_ID]: makeSession(SESSION_ID) });
    expect(rootStore.sessions[OTHER_ID]).toBeUndefined();
    expect(rootStore.terminal_title[OTHER_ID]).toBeUndefined();
    expect(rootStore.last_activity[OTHER_ID]).toBeUndefined();
    expect(rootStore.sessions[SESSION_ID]).toBe(kept);
  });

  test("an in-flight optimistic spawn survives a snapshot that omits it", () => {
    resetSessions();
    applySessionsSnapshot({ [SESSION_ID]: makeSession(SESSION_ID) });
    const pending = beginOptimisticSpawn(rootStore.sessions[SESSION_ID]! as Session);

    // The re-hydration predates the spawn: its id is not real yet.
    applySessionsSnapshot({ [SESSION_ID]: makeSession(SESSION_ID) });
    expect(rootStore.sessions[pending]).toBeDefined();

    // Once the spawn settles, the same absent id is a real deletion.
    endOptimisticSpawn(pending);
    applySessionsSnapshot({ [SESSION_ID]: makeSession(SESSION_ID) });
    expect(rootStore.sessions[pending]).toBeUndefined();
  });

  test("the event fold shares the convention: metadata updates in place", () => {
    resetSessions();
    foldEventIntoStore(makeOpenedEvent());
    const mounted = rootStore.sessions[SESSION_ID]!;
    foldEventIntoStore({ kind: "cwd", session_id: SESSION_ID, cwd: "/folded", ts: 1500 });
    expect(rootStore.sessions[SESSION_ID]).toBe(mounted);
    expect(rootStore.sessions[SESSION_ID]!.cwd).toBe("/folded");
  });
});

// ──────────────────────────────────────────────────────────────────────
// FOLD SAFETY + PRODUCER GENERATION — the Sync stream hands the projector
// decoded but unverified events. A rejected or reordered one must never unwind
// a LIVE session (that is a blank pane no event asked for), while a respawn or a
// worker reconcile snapshot must replay the tab's positive viewport owner so the
// new keeper core produces a full frame without a remount.
// ──────────────────────────────────────────────────────────────────────

describe("fold safety and producer-generation replay", () => {
  test("every wire-decoded event kind passes the boundary gate", () => {
    resetSessions();
    // protoToEvent casts rather than validates, so this gate is the FIRST check a
    // live event meets. If any real kind failed it, the SPA would silently stop
    // projecting — assert the whole vocabulary survives the wire round trip.
    const events: SessionEvent[] = [
      makeOpenedEvent(),
      { kind: "closed", session_id: SESSION_ID, exit_code: 0, ts: 1010 },
      { kind: "attached", session_id: SESSION_ID, ts: 1020 },
      { kind: "detached", session_id: SESSION_ID, ts: 1030 },
      { kind: "cwd", session_id: SESSION_ID, cwd: "/wire", ts: 1040 },
      { kind: "workspace_assigned", session_id: SESSION_ID, workspace_id: WS_ID, ts: 1050 },
      { kind: "snapshot", worker_fp: FP, sessions: [makeSession(SESSION_ID)], ts: 1060 },
      { kind: "respawned", session_id: SESSION_ID, new_channel: asChannelId(8), ts: 1070 },
      { kind: "renamed", session_id: SESSION_ID, custom_title: "build", ts: 1080 },
      { kind: "git", session_id: SESSION_ID, branch: "main", remote: "o/r", ts: 1090 },
      { kind: "pr", session_id: SESSION_ID, number: 7, state: "open", checks: "passing", url: "u", ts: 1100 },
      { kind: "ports", session_id: SESSION_ID, ports: [5173], ts: 1110 },
    ];
    for (const event of events) {
      const decoded = protoToEvent(eventToProto(event, 1));
      expect(decoded).not.toBeNull();
      expect(SessionEventSchema.safeParse(decoded).success).toBe(true);
    }

    // And the projector really applies a wire-decoded event.
    const opened = protoToEvent(eventToProto(makeOpenedEvent(), 2))!;
    foldEventIntoStore(opened);
    expect(rootStore.sessions[SESSION_ID]).toBeDefined();
  });

  test("a Zod-rejected event leaves the live session and its volatile slices intact", () => {
    resetSessions();
    foldEventIntoStore(makeOpenedEvent());
    setRootStore("terminal_title", SESSION_ID, "vim");
    setRootStore("last_activity", SESSION_ID, 42);

    // Wrong `exit_code` type: the fold's `closed` branch needs only session_id,
    // so without boundary validation this would delete a live session.
    const malformedClosed = {
      kind: "closed",
      session_id: SESSION_ID,
      exit_code: "boom",
      ts: 4000,
    } as unknown as SessionEvent;
    foldEventIntoStore(malformedClosed);

    expect(rootStore.sessions[SESSION_ID]).toBeDefined();
    expect(rootStore.terminal_title[SESSION_ID]).toBe("vim");
    expect(rootStore.last_activity[SESSION_ID]).toBe(42);

    // A malformed respawn cannot move the session onto a bogus channel either.
    const malformedRespawn = {
      kind: "respawned",
      session_id: SESSION_ID,
      new_channel: "not-a-channel",
      ts: 4100,
    } as unknown as SessionEvent;
    foldEventIntoStore(malformedRespawn);
    expect(rootStore.sessions[SESSION_ID]!.channel).toBe(asChannelId(1));
  });

  test("a valid close still deletes the session with its volatile slices", () => {
    resetSessions();
    foldEventIntoStore(makeOpenedEvent());
    setRootStore("terminal_title", SESSION_ID, "vim");
    setRootStore("last_activity", SESSION_ID, 42);

    foldEventIntoStore({ kind: "closed", session_id: SESSION_ID, exit_code: 0, ts: 4200 });

    expect(rootStore.sessions[SESSION_ID]).toBeUndefined();
    expect(rootStore.terminal_title[SESSION_ID]).toBeUndefined();
    expect(rootStore.last_activity[SESSION_ID]).toBeUndefined();
  });

  test("a snapshot that omits a live session keeps it and its slices", () => {
    resetSessions();
    const ghostId = asSessionId("00000000-0000-0000-0000-000000000008");
    foldEventIntoStore(makeOpenedEvent({ session_id: ghostId }));
    foldEventIntoStore(makeOpenedEvent({ session_id: SESSION_ID }));
    setRootStore("terminal_title", ghostId, "htop");

    foldEventIntoStore({
      kind: "snapshot",
      worker_fp: FP,
      sessions: [makeSession(SESSION_ID, { channel: asChannelId(3) })],
      ts: 4300,
    });

    expect(rootStore.sessions[ghostId]).toBeDefined();
    expect(rootStore.terminal_title[ghostId]).toBe("htop");
    expect(rootStore.sessions[SESSION_ID]!.channel).toBe(asChannelId(3));
  });

  test("respawn and reconcile snapshot replay a positive viewport owner at held zero", () => {
    resetSessions();
    _resetTerminalOutboundForTest();
    foldEventIntoStore(makeOpenedEvent());
    const owner = acquireTerminalViewportOwner(SESSION_ID);
    owner.claim({ cols: 120, rows: 40, cause: 1, heldCellSeq: 31n });
    expect(terminalOutboundSnapshot(SESSION_ID).claim.desired?.held_cell_seq).toBe("31");

    foldEventIntoStore({ kind: "respawned", session_id: SESSION_ID, new_channel: asChannelId(44), ts: 4400 });
    expect(terminalOutboundSnapshot(SESSION_ID).claim.desired?.held_cell_seq).toBe("0");

    // A later heartbeat re-states the tab's own watermark; the announcing
    // worker's reconcile snapshot forces zero again.
    owner.heartbeat(77n);
    expect(terminalOutboundSnapshot(SESSION_ID).claim.desired?.held_cell_seq).toBe("77");
    foldEventIntoStore({
      kind: "snapshot",
      worker_fp: FP,
      sessions: [makeSession(SESSION_ID, { channel: asChannelId(44) })],
      ts: 4500,
    });
    expect(terminalOutboundSnapshot(SESSION_ID).claim.desired?.held_cell_seq).toBe("0");
    owner.dispose();
    _resetTerminalOutboundForTest();
  });

  test("a tab with no viewport owner is untouched by a producer generation change", () => {
    resetSessions();
    _resetTerminalOutboundForTest();
    foldEventIntoStore(makeOpenedEvent());
    foldEventIntoStore({ kind: "respawned", session_id: SESSION_ID, new_channel: asChannelId(45), ts: 4600 });
    expect(terminalOutboundSnapshot(SESSION_ID).claim.desired).toBeNull();
    expect(rootStore.sessions[SESSION_ID]!.channel).toBe(asChannelId(45));
  });
});
