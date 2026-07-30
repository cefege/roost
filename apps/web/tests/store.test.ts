// Invariant tests for the web store projection layer: shared-fold determinism,
// visible spawned rows, and agreement with the SPA projector.

import { expect, test, describe } from "bun:test";
import { foldEvent, foldAll, asWorkerFp, asSessionId, asChannelId, asWorkspaceId } from "@roost/shared/wire";
import type { SessionEvent, Session } from "@roost/shared/wire";
import { foldEventIntoStore } from "../src/store/projector.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";

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
