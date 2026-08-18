// What the terminal core THREW AWAY, per session.
//
// The core keeps a 32-entry ring of CSI sequences its dispatcher ignored. It is
// the only evidence that exists for "the core silently dropped this sequence" —
// the class behind "my TUI renders wrong in Roost but fine in iTerm" — and the
// ring is NEVER cleared, so the whole contract is about not saying the same
// thing twice:
//
//   * one Tier-1 signal per DISTINCT sequence per core instance, no matter how
//     many frames are emitted afterwards or how often the app repeats it;
//   * a rebuild mints a fresh core with an empty ring, so its entries are a new
//     core's report and are stated again;
//   * a session that never trips the dispatcher creates no state at all;
//   * the accumulator and the snapshot stay bounded by the ring's own capacity,
//     and the per-channel cooldown keeps a spraying TUI to one log line.
//
// The core here is the real digest-pinned one, fed through the real PTY
// entrypoint (emitUpstreamChunk → ring + core) and the real emit path.

import { describe, test, expect, afterEach, setSystemTime } from "bun:test";
import { asChannelId, asSessionId, asWorkerFp } from "@roost/shared";
import { setSignalSink } from "@roost/shared/diag";
import { initCellEmitState } from "@roost/shared/cell";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { rebuildTerminalCore } from "../src/session-resize-capture.ts";
import {
  UNHANDLED_SEQ_MAX,
  unhandledSequenceSnapshot,
  type UnhandledSequenceSnapshot,
} from "../src/session-unhandled-seq.ts";

const SID_BY_CID: Record<number, string> = {
  3: "00000000-0000-4000-8000-0000000000f1",
  4: "00000000-0000-4000-8000-0000000000f2",
  5: "00000000-0000-4000-8000-0000000000f3",
  6: "00000000-0000-4000-8000-0000000000f4",
  7: "00000000-0000-4000-8000-0000000000f5",
};
const COLS = 40;
const ROWS = 6;

// Two sequences the pinned core's CSI dispatcher does not implement, confirmed
// by the assertions below rather than assumed: DECSCUSR (cursor style) and a
// private-prefixed final. Both are exactly the "iTerm honours it, Roost ignores
// it" shape this signal exists to name.
const DECSCUSR = "\x1b[2 q";
const PRIVATE_W = "\x1b[>10;20;30W";

interface Harness {
  mgr: SessionManager;
  rec: SessionShellRecord;
  sid: string;
  signals: Array<Record<string, unknown>>;
}

/** One channel id per test: the Tier-1 cooldown is per kind+channel and lives in
 *  module state, so sharing a channel across tests would let one test's line
 *  swallow the next test's. */
async function harness(cid: number): Promise<Harness> {
  const sid = SID_BY_CID[cid]!;
  const signals: Array<Record<string, unknown>> = [];
  setSignalSink((record) => signals.push(record));
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: () => {},
  });
  const wtermCore = await createWtermCore(COLS, ROWS);
  const rec = {
    sessionId: asSessionId(sid),
    channelId: asChannelId(cid),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    sb_origin_pin: null,
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "unhandled",
    cell_emit: initCellEmitState("unhandled-grid"),
    lastPtyOutMs: 0,
  } as unknown as SessionShellRecord;
  mgr.sessions.set(cid, rec);
  return { mgr, rec, sid, signals };
}

/** The real PTY entrypoint: ring + core, no viewer, so nothing emits until the
 *  test says so. */
function feed(mgr: SessionManager, cid: number, payload: string): void {
  mgr.emitUpstreamChunk(cid, Buffer.from(payload, "binary"));
}

function snapshotOf(rec: SessionShellRecord): UnhandledSequenceSnapshot {
  const snap = unhandledSequenceSnapshot(rec, rec.wtermCore);
  if (snap === null) throw new Error("expected the core to have logged something");
  return snap;
}

describe("core-reported unhandled sequences", () => {
  afterEach(() => {
    setSignalSink(null);
    setSystemTime();
  });

  test("one signal per distinct sequence, never a second time for the same core", async () => {
    // The signal channel coalesces one kind+channel for 10 s, so the second
    // distinct sequence is separated by a clock step instead of a real sleep.
    setSystemTime(new Date("2026-08-18T00:00:00Z"));
    const cid = 3;
    const { mgr, rec, sid, signals } = await harness(cid);

    feed(mgr, cid, "plain output\r\n");
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(0);
    expect(rec.unhandled).toBeUndefined();

    feed(mgr, cid, DECSCUSR);
    mgr.emitCellFrame(cid, true);
    mgr.emitCellFrame(cid, true);
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(1);
    expect(signals[0]).toMatchObject({
      evt: "terminal.unhandled_sequence",
      sid,
      channel_id: cid,
      final: "q",
      private: "",
      param_count: 1,
      params: "2",
      distinct: 1,
      logged_total: 1,
      ring_dropped: 0,
    });

    // The app repeating itself is not news, even though the ring's total grows.
    setSystemTime(new Date("2026-08-18T00:00:11Z"));
    feed(mgr, cid, DECSCUSR + DECSCUSR);
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(1);
    expect(rec.unhandled?.consumed).toBe(3);
    expect(snapshotOf(rec).entries).toHaveLength(1);

    // A genuinely different sequence is.
    feed(mgr, cid, PRIVATE_W);
    mgr.emitCellFrame(cid, true);
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(2);
    expect(signals[1]).toMatchObject({
      evt: "terminal.unhandled_sequence",
      final: "W",
      private: ">",
      param_count: 3,
      params: "10;20;30",
      distinct: 2,
      logged_total: 4,
    });

    const snap = snapshotOf(rec);
    expect(snap.logged_total).toBe(4);
    expect(snap.ring_dropped).toBe(0);
    expect(snap.capped).toBe(false);
    expect(snap.entries.map((entry) => entry.final)).toEqual(["q", "W"]);
    expect(snap.entries[0]!.params).toEqual([2]);
    expect(snap.entries[1]!.params).toEqual([10, 20, 30]);
    // Monotonic, per entry, in arrival order — an age an operator can trust
    // across a host clock step.
    expect(snap.entries[0]!.first_seen_mono_ms).toBeGreaterThan(0);
    expect(snap.entries[1]!.first_seen_mono_ms)
      .toBeGreaterThanOrEqual(snap.entries[0]!.first_seen_mono_ms);
  });

  test("a core rebuild reports the fresh core's ring again", async () => {
    setSystemTime(new Date("2026-08-18T00:01:00Z"));
    const cid = 4;
    const { mgr, rec, signals } = await harness(cid);

    feed(mgr, cid, DECSCUSR);
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(1);
    const firstSeen = snapshotOf(rec).entries[0]!.first_seen_mono_ms;

    // The rebuild replays the retained ring into a brand-new core, whose own ring
    // starts empty and logs the same sequence afresh. A mark held across that
    // swap would have suppressed it forever.
    setSystemTime(new Date("2026-08-18T00:01:11Z"));
    expect(await rebuildTerminalCore(mgr, cid, COLS + 20, ROWS, null)).toBe(true);
    expect(rec.unhandled).toBeUndefined();

    mgr.emitCellFrame(cid, true);
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(2);
    expect(signals[1]).toMatchObject({
      evt: "terminal.unhandled_sequence",
      final: "q",
      distinct: 1,
      logged_total: 1,
    });
    const snap = snapshotOf(rec);
    expect(snap.entries).toHaveLength(1);
    expect(snap.entries[0]!.first_seen_mono_ms).toBeGreaterThanOrEqual(firstSeen);
  });

  test("a spraying TUI stays bounded: 32 entries, one log line, drops counted", async () => {
    setSystemTime(new Date("2026-08-18T00:02:00Z"));
    const cid = 5;
    const { mgr, rec, signals } = await harness(cid);

    // 40 distinct unhandled sequences in one chunk: more than the core's ring can
    // retain, so 8 are overwritten before the worker ever looks at them.
    let spray = "";
    for (let i = 1; i <= 40; i++) spray += `\x1b[${i}Y`;
    feed(mgr, cid, spray);
    mgr.emitCellFrame(cid, true);

    const snap = snapshotOf(rec);
    expect(snap.logged_total).toBe(40);
    expect(snap.entries).toHaveLength(UNHANDLED_SEQ_MAX);
    expect(snap.capped).toBe(true);
    expect(snap.ring_dropped).toBe(40 - UNHANDLED_SEQ_MAX);
    // The window the core still held: parameters 9..40, oldest first.
    expect(snap.entries[0]!.params).toEqual([9]);
    expect(snap.entries.at(-1)!.params).toEqual([40]);
    // Per-channel cooldown: 32 distinct sequences, ONE line in the always-on
    // channel. The snapshot above is where the rest is read.
    expect(signals).toHaveLength(1);

    // Capped means capped: further frames neither grow the list nor re-report.
    setSystemTime(new Date("2026-08-18T00:02:11Z"));
    feed(mgr, cid, "\x1b[99Y");
    mgr.emitCellFrame(cid, true);
    expect(signals).toHaveLength(1);
    expect(snapshotOf(rec).entries).toHaveLength(UNHANDLED_SEQ_MAX);
    expect(snapshotOf(rec).logged_total).toBe(41);
  });

  test("the per-session diagnostic snapshot answers for a pane that emits nothing", async () => {
    setSystemTime(new Date("2026-08-18T00:03:00Z"));
    const cid = 6;
    const { mgr, rec, sid } = await harness(cid);

    // No frame is ever emitted here — a parked pane. The snapshot samples the
    // core itself, which is the only way that session is inspectable.
    feed(mgr, cid, PRIVATE_W);
    expect(rec.unhandled).toBeUndefined();

    const sessions = mgr.diagSnapshot().sessions as Record<
      string,
      { terminal: { unhandled_sequences: UnhandledSequenceSnapshot | null } }
    >;
    const reported = sessions[sid]!.terminal.unhandled_sequences;
    expect(reported?.entries).toHaveLength(1);
    expect(reported?.entries[0]).toMatchObject({
      final: "W", private: ">", param_count: 3, params: [10, 20, 30],
    });
  });

  test("a session the core fully understands reports null and holds no state", async () => {
    setSystemTime(new Date("2026-08-18T00:04:00Z"));
    const cid = 7;
    const { mgr, rec, sid, signals } = await harness(cid);

    // Plain text, a real cursor move, a real SGR, a real erase: all implemented.
    feed(mgr, cid, "\x1b[2J\x1b[1;1H\x1b[1;31mred\x1b[0m\r\nplain\r\n");
    for (let i = 0; i < 5; i++) mgr.emitCellFrame(cid, true);

    expect(signals).toHaveLength(0);
    expect(rec.unhandled).toBeUndefined();
    expect(unhandledSequenceSnapshot(rec, rec.wtermCore)).toBeNull();
    const sessions = mgr.diagSnapshot().sessions as Record<
      string,
      { terminal: { unhandled_sequences: UnhandledSequenceSnapshot | null } }
    >;
    expect(sessions[sid]!.terminal.unhandled_sequences).toBeNull();
  });
});
