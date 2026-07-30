// phase-24f — foldEvent equivalence property test.
//
// Invariant: `foldEvent` from @roost/shared/wire is the SINGLE projection
// function called from THREE places — coord projector (event-log.ts),
// SPA store projector (apps/web/src/store/projector.ts), worker
// snapshot reconciliation. If any caller drifts (caches a stale ref,
// imports a forked copy, reimplements the logic), projections diverge
// silently → L11 row `feedback_solid_setstore_record_replace.md` class
// of bug.
//
// Two properties verified via fast-check on random SessionEvent[]:
//
//   P1 — DETERMINISM: foldAll(events) returns byte-equal JSON across
//        repeated calls. No hidden state, no Date.now()/Math.random()
//        sneaking into the projection.
//   P2 — INCREMENTAL ≡ BATCH: starting from {}, applying events
//        one-by-one yields the same Map as foldAll(events).
//
// Adding a new SessionEvent variant or changing foldEvent shape MUST
// keep both properties green; if not, every caller of foldEvent is
// silently broken.

import { describe, test, expect } from "bun:test";
import fc from "fast-check";
import {
  foldAll,
  foldEvent,
  type SessionEvent,
  type Session,
  type WorkerFp,
  type SessionId,
  type ChannelId,
} from "../src/wire/index.ts";

// ─── arbitraries ─────────────────────────────────────────────────────

const sessionIdArb = fc.uuid().map((s) => s as SessionId);
const workerFpArb = fc.constantFrom(
  "a".repeat(64) as WorkerFp,
  "b".repeat(64) as WorkerFp,
  "c".repeat(64) as WorkerFp,
);
const channelArb = fc.integer({ min: 1, max: 200 }).map((n) => n as ChannelId);
const cwdArb = fc.constantFrom("/", "/tmp", "/Users/you", "/var/log");
const sessionKindArb = fc.constant("shell" as const);
const tsArb = fc.integer({ min: 1, max: 2_000_000_000_000 });

function openedEvent(id: SessionId, fp: WorkerFp, ts: number): SessionEvent {
  return {
    kind: "opened",
    session_id: id,
    worker_fp: fp,
    channel: 1 as ChannelId,
    session_kind: "shell",
    cwd: "/",
    ts,
  };
}

const eventArb = (idPool: SessionId[]): fc.Arbitrary<SessionEvent> =>
  fc.oneof(
    fc.record({
      kind: fc.constant("opened" as const),
      session_id: fc.constantFrom(...idPool),
      worker_fp: workerFpArb,
      channel: channelArb,
      session_kind: sessionKindArb,
      cwd: cwdArb,
      ts: tsArb,
    }),
    fc.record({
      kind: fc.constant("closed" as const),
      session_id: fc.constantFrom(...idPool),
      exit_code: fc.option(fc.integer({ min: 0, max: 255 }), { nil: null }),
      ts: tsArb,
    }),
    fc.record({
      kind: fc.constant("cwd" as const),
      session_id: fc.constantFrom(...idPool),
      cwd: cwdArb,
      ts: tsArb,
    }),
    fc.record({
      kind: fc.constant("workspace_assigned" as const),
      session_id: fc.constantFrom(...idPool),
      workspace_id: fc.constant(null),
      ts: tsArb,
    }),
    fc.record({
      kind: fc.constant("attached" as const),
      session_id: fc.constantFrom(...idPool),
      ts: tsArb,
    }),
    fc.record({
      kind: fc.constant("detached" as const),
      session_id: fc.constantFrom(...idPool),
      ts: tsArb,
    }),
  );

const eventLogArb = fc.array(sessionIdArb, { minLength: 1, maxLength: 6 }).chain((ids) =>
  fc.array(eventArb(ids), { minLength: 0, maxLength: 50 }),
);

// ─── projection canonicalisation ─────────────────────────────────────
// Map<string, Session> → stable JSON. Sort by id so insertion-order
// differences don't read as projection drift.
function canonical(map: Map<string, Session>): string {
  const arr = Array.from(map.values()).sort((a, b) => a.id.localeCompare(b.id));
  return JSON.stringify(arr);
}

// ─── tests ───────────────────────────────────────────────────────────

describe("phase-24f foldEvent equivalence", () => {
  test("P1 determinism: foldAll is a pure function of its input", () => {
    fc.assert(
      fc.property(eventLogArb, (events) => {
        const a = canonical(foldAll(events));
        const b = canonical(foldAll(events));
        return a === b;
      }),
      { numRuns: 200 },
    );
  });

  test("P2 incremental == batch: per-event reduce matches foldAll", () => {
    fc.assert(
      fc.property(eventLogArb, (events) => {
        const batch = canonical(foldAll(events));
        let acc = new Map<string, Session>();
        for (const e of events) acc = foldEvent(acc, e);
        const incremental = canonical(acc);
        return batch === incremental;
      }),
      { numRuns: 200 },
    );
  });

  test("snapshot for worker A leaves worker B sessions untouched", () => {
    const a = "a".repeat(64) as WorkerFp;
    const b = "b".repeat(64) as WorkerFp;
    const idA1 = "11111111-1111-1111-1111-111111111111" as SessionId;
    const idA2 = "22222222-2222-2222-2222-222222222222" as SessionId;
    const idB1 = "33333333-3333-3333-3333-333333333333" as SessionId;
    const events: SessionEvent[] = [
      openedEvent(idA1, a, 1),
      openedEvent(idA2, a, 2),
      openedEvent(idB1, b, 3),
      // Worker A reconnects, only reports idA1.
      {
        kind: "snapshot",
        worker_fp: a,
        sessions: [{
          id: idA1, worker_fp: a, channel: 1 as ChannelId, kind: "shell",
          cwd: "/", workspace_id: null, status: "open",
          created_at: 1, closed_at: null, custom_title: null,
        }],
        ts: 4,
      },
    ];
    const m = foldAll(events);
    expect(m.has(idA1)).toBe(true);
    // Breadcrumb model: idA2 was open on worker A but absent from A's snapshot
    // (e.g. its PTY died in a restart). It is NOT pruned — it persists as an
    // offline breadcrumb row until an explicit `closed`/✕. Only real PTY-exit
    // `closed` events delete a session now.
    expect(m.has(idA2)).toBe(true);
    expect(m.has(idB1)).toBe(true);
  });

  test("breadcrumb lifecycle: restart-snapshot keeps → respawned re-binds → closed removes", () => {
    const id = "55555555-5555-5555-5555-555555555555" as SessionId;
    const w = "c".repeat(64) as WorkerFp;
    const restartSnap: SessionEvent = { kind: "snapshot", worker_fp: w, sessions: [], ts: 2 };
    // Worker restart: its boot snapshot omits the session → the row PERSISTS.
    expect(foldAll([openedEvent(id, w, 1), restartSnap]).has(id)).toBe(true);
    // Respawn re-binds the surviving row to a fresh keeper channel.
    const afterRespawn = foldAll([
      openedEvent(id, w, 1), restartSnap,
      { kind: "respawned", session_id: id, new_channel: 7 as ChannelId, ts: 3 },
    ]);
    expect(afterRespawn.get(id)!.channel).toBe(7 as ChannelId);
    // An explicit `closed` (real PTY exit / the user's ✕) is the ONLY remover.
    expect(foldAll([
      openedEvent(id, w, 1), restartSnap,
      { kind: "closed", session_id: id, exit_code: 0, ts: 4 },
    ]).has(id)).toBe(false);
  });

  test("closed without prior opened is a no-op", () => {
    const id = "44444444-4444-4444-4444-444444444444" as SessionId;
    const events: SessionEvent[] = [
      { kind: "closed", session_id: id, exit_code: 0, ts: 1 },
    ];
    expect(foldAll(events).size).toBe(0);
  });

});
