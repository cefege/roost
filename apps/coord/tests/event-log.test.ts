// event-log invariant tests. fast-check property: fold determinism +
// snapshot ghost-close invariant. R5.2.

import { describe, test, expect } from "bun:test";
import fc from "fast-check";
import { foldAll, foldEvent } from "@roost/shared/wire";
import type { SessionEvent, Session } from "@roost/shared/wire";
import { asSessionId, asWorkerFp, asChannelId, asWorkspaceId } from "@roost/shared/wire";

const FP = asWorkerFp("a".repeat(64));
const SID = asSessionId("00000000-0000-4000-8000-000000000001");
const SID2 = asSessionId("00000000-0000-4000-8000-000000000002");
const CH = asChannelId(1);

function openEvent(sid = SID, ts = 1000): SessionEvent {
  return { kind: "opened", session_id: sid, worker_fp: FP, channel: CH, session_kind: "shell", cwd: "/home", ts };
}

function closeEvent(sid = SID, ts = 2000): SessionEvent {
  return { kind: "closed", session_id: sid, exit_code: 0, ts };
}

// ─── determinism property ──────────────────────────────────────────────

describe("foldAll determinism", () => {
  test("same events → same result", () => {
    fc.assert(
      fc.property(
        // A sequence of valid events for a single session.
        fc.integer({ min: 0, max: 10 }),
        (n) => {
          const events: SessionEvent[] = [openEvent()];
          for (let i = 0; i < n; i++) {
            events.push({ kind: "cwd", session_id: SID, cwd: `/home/${i}`, ts: 1001 + i });
          }
          events.push(closeEvent());

          const r1 = foldAll(events);
          const r2 = foldAll(events);

          // Deep structural equality.
          expect(JSON.stringify([...r1.entries()].sort())).toBe(JSON.stringify([...r2.entries()].sort()));
        },
      ),
      { numRuns: 100 },
    );
  });

  test("fold prefix then suffix = fold all", () => {
    fc.assert(
      fc.property(
        fc.integer({ min: 1, max: 8 }),
        (split) => {
          const events: SessionEvent[] = [
            openEvent(),
            { kind: "cwd", session_id: SID, cwd: "/a", ts: 1001 },
            { kind: "cwd", session_id: SID, cwd: "/b", ts: 1002 },
            { kind: "cwd", session_id: SID, cwd: "/c", ts: 1003 },
            closeEvent(SID, 2000),
          ];
          const clamp = Math.min(split, events.length);
          const allAtOnce = foldAll(events);
          const partial = foldAll(events.slice(0, clamp));
          const resumed = events.slice(clamp).reduce(foldEvent, partial);
          expect(JSON.stringify([...allAtOnce.entries()].sort())).toBe(
            JSON.stringify([...resumed.entries()].sort()),
          );
        },
      ),
      { numRuns: 100 },
    );
  });
});

// ─── snapshot reconciliation invariant ────────────────────────────────

describe("snapshot reconciliation", () => {
  test("snapshot installs announced sessions", () => {
    const s: Session = {
      id: SID,
      worker_fp: FP,
      channel: CH,
      kind: "shell",
      cwd: "/repo",
      workspace_id: null,
      status: "open",
      created_at: 1000,
      closed_at: null,
      custom_title: null,
    };
    const snap: SessionEvent = { kind: "snapshot", worker_fp: FP, sessions: [s], ts: 2000 };
    const result = foldAll([snap]);
    expect(result.get(SID)?.cwd).toBe("/repo");
  });

  test("snapshot KEEPS sessions missing from the snap (breadcrumb model)", () => {
    // Session S1 opened, then snapshot only has S2.
    const s1Events: SessionEvent[] = [openEvent(SID, 1000)];
    const s2: Session = {
      id: SID2,
      worker_fp: FP,
      channel: asChannelId(2),
      kind: "shell",
      cwd: "/other",
      workspace_id: null,
      status: "open",
      created_at: 1500,
      closed_at: null,
      custom_title: null,
    };
    const snap: SessionEvent = { kind: "snapshot", worker_fp: FP, sessions: [s2], ts: 3000 };
    const result = foldAll([...s1Events, snap]);
    // Breadcrumb model: S1 is absent from the snapshot but PERSISTS (offline row
    // until an explicit `closed`/✕) so a worker restart doesn't wipe the sidebar.
    // S2 installed.
    expect(result.has(SID)).toBe(true);
    expect(result.get(SID2)?.cwd).toBe("/other");
  });

  test("snapshot preserves immutable creation and coordinator-owned fields", () => {
    const workspaceId = asWorkspaceId("00000000-0000-4000-8000-000000000099");
    const before = foldAll([
      openEvent(SID, 1000),
      {
        kind: "workspace_assigned",
        session_id: SID,
        workspace_id: workspaceId,
        ts: 1001,
      },
      {
        kind: "renamed",
        session_id: SID,
        custom_title: "kept title",
        ts: 1002,
      },
    ]);
    const announced: Session = {
      ...before.get(SID)!,
      channel: asChannelId(9),
      cwd: "/current",
      created_at: 9000,
      spawn_cwd: "/rewritten",
      workspace_id: asWorkspaceId("00000000-0000-4000-8000-000000000088"),
      custom_title: "rewritten title",
    };
    const result = foldEvent(before, {
      kind: "snapshot",
      worker_fp: FP,
      sessions: [announced],
      ts: 10_000,
    });
    expect(result.get(SID)).toMatchObject({
      channel: 9,
      cwd: "/current",
      created_at: 1000,
      spawn_cwd: "/home",
      workspace_id: workspaceId,
      custom_title: "kept title",
    });
  });

  test("snapshot from different worker does not affect other worker sessions", () => {
    const FP2 = asWorkerFp("b".repeat(64));
    const s1: SessionEvent = openEvent(SID, 1000);
    const snap: SessionEvent = { kind: "snapshot", worker_fp: FP2, sessions: [], ts: 2000 };
    const result = foldAll([s1, snap]);
    // FP's session unaffected; FP2 snapshot empty.
    expect(result.get(SID)?.status).toBe("open");
  });
});

// ─── projection correctness ───────────────────────────────────────────

describe("projection correctness", () => {
  test("opened → closed deletes the session (no closed limbo)", () => {
    const events: SessionEvent[] = [openEvent(), closeEvent()];
    const result = foldAll(events);
    // closed removes the row from the projection rather than parking it.
    expect(result.has(SID)).toBe(false);
  });

  test("cwd update reflected in projection", () => {
    const events: SessionEvent[] = [
      openEvent(),
      { kind: "cwd", session_id: SID, cwd: "/new", ts: 1500 },
    ];
    expect(foldAll(events).get(SID)?.cwd).toBe("/new");
  });



});
