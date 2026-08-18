// The ordered capability-reply lane (src/terminal-query-reply.ts) plus its live
// wiring through appendScrollback.
//
// Contracts under test, all of them things a real client hangs on:
//   - a probe split across pty chunk boundaries at ANY offset is answered
//     exactly once, and the core still receives every byte exactly once;
//   - N probes in one chunk produce N replies, in the order the probes appeared;
//   - native (core-answered) and synthesized replies share ONE ordered lane, so
//     `ESC[c` then `ESC[6n` is never answered CPR-first;
//   - the carry is bounded, per session, and never fabricates a probe out of two
//     unrelated chunks.
//
// Real claude sends Primary DA (ESC[c) x2 + XTVERSION (ESC[>0q) at startup
// (captured 2026-07-05) and the core answers neither.

import { describe, test, expect, afterEach } from "bun:test";
import { WasmBridge } from "@wterm/core";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { initCellEmitState } from "@roost/shared/cell";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import {
  installResizeCapture,
  markResizeBoundary,
  rebuildTerminalCore,
} from "../src/session-resize-capture.ts";
import { MuxFrameType } from "../src/keeper/protocol-v2.ts";
import { installFakeKeeper, type FakeKeeper } from "./keeper-fake-pool.ts";
import {
  answerQueries,
  QUERY_CARRY_MAX,
  type QueryCarry,
  type QueryCore,
} from "../src/terminal-query-reply.ts";

const DA = "\x1b[?1;2c";
const XTV = "\x1bP>|wterm(roost)\x1b\\";
const CPR = "\x1b[6n";
const enc = (s: string) => new TextEncoder().encode(s);
const dec = new TextDecoder();

/** The real WasmBridge splits a write into 8192-byte pieces and calls
 *  `afterChunk` after each; the reply FIFO is bounded, so a lane that only
 *  drains after the whole write loses replies from the earlier pieces. */
const CORE_WRITE_CHUNK = 8192;

/** A core that answers exactly the probe the real one answers and nothing else.
 *  Every byte ever written is kept in one buffer, so a probe split across
 *  writeRaw calls completes exactly like a real parser's own carry would, and
 *  each completed ESC[6n queues one positionally distinct reply — which is what
 *  makes an ordering assertion readable rather than a count. */
class ProbeCore implements QueryCore {
  /** One entry per writeRaw call: the segmentation the lane chose. */
  readonly writes: string[] = [];
  private seen = "";
  private answered = 0;
  private issued = 0;
  private queue: string[] = [];

  constructor(private readonly capacity = 64) {}

  writeRaw(data: Uint8Array, afterChunk?: () => void): void {
    this.writes.push(dec.decode(data));
    for (let at = 0; at < data.length; at += CORE_WRITE_CHUNK) {
      this.seen += dec.decode(data.subarray(at, Math.min(at + CORE_WRITE_CHUNK, data.length)));
      let from = 0;
      for (;;) {
        const hit = this.seen.indexOf(CPR, from);
        if (hit < 0) break;
        from = hit + CPR.length;
        if (from <= this.answered) continue;
        this.answered = from;
        this.issued++;
        this.queue.push(`\x1b[${this.issued};1R`);
        // A bounded FIFO: the oldest reply is what a late drain loses.
        if (this.queue.length > this.capacity) this.queue.shift();
      }
      afterChunk?.();
    }
  }

  getResponse(): string | null {
    return this.queue.shift() ?? null;
  }
}

function freshState(): QueryCarry {
  return { query_carry: new Uint8Array(0) };
}

/** Feed one chunk and return what the pty is owed. */
function feed(state: QueryCarry, core: ProbeCore, chunk: string): string {
  return answerQueries(state, core, enc(chunk)).bytes;
}

describe("probe tokenizer", () => {
  test("answers Primary DA, its explicit-0 form, and XTVERSION", () => {
    expect(feed(freshState(), new ProbeCore(), "\x1b[c")).toBe(DA);
    expect(feed(freshState(), new ProbeCore(), "\x1b[0c")).toBe(DA);
    expect(feed(freshState(), new ProbeCore(), "\x1b[>0q")).toBe(XTV);
    expect(feed(freshState(), new ProbeCore(), "\x1b[>q")).toBe(XTV);
  });

  test("recognises the probes Roost does not answer without answering them", () => {
    // DA2, DECRQM, DECSCUSR and a non-zero XTVERSION parameter are complete CSI
    // sequences that are NOT Primary DA / XTVERSION. Tokenizing them is what
    // keeps a following probe findable; answering them would be a lie.
    const core = new ProbeCore();
    const state = freshState();
    expect(feed(state, core, "\x1b[>c\x1b[>0c\x1b[?2026$p\x1b[ q\x1b[>1q\x1b[?c")).toBe("");
    // ...and the stream is still tokenized correctly afterwards.
    expect(feed(state, core, "\x1b[c")).toBe(DA);
  });

  test("no false match on cursor moves, SGR or plain text", () => {
    const core = new ProbeCore();
    expect(feed(freshState(), core, "\x1b[12G\x1b[38;2;1;2;3m\x1b[0m")).toBe("");
    expect(feed(freshState(), core, "hello world\n")).toBe("");
    expect(feed(freshState(), core, "")).toBe("");
  });

  test("real claude startup burst → DA x2 + XTVERSION, in order", () => {
    const burst = "\x1b[c\x1b[?1049h\x1b[2J\x1b[c\x1b[>0q\x1b[?2004h";
    expect(feed(freshState(), new ProbeCore(), burst)).toBe(DA + DA + XTV);
  });
});

describe("ordered lane", () => {
  test("N probes in one chunk produce N replies, in probe order", () => {
    const core = new ProbeCore();
    const chunk = "\x1b[c\x1b[c\x1b[>0q\x1b[6n\x1b[0c\x1b[6n";
    expect(feed(freshState(), core, chunk))
      .toBe(DA + DA + XTV + "\x1b[1;1R" + DA + "\x1b[2;1R");
    expect(core.writes.join("")).toBe(chunk);
  });

  test("a native reply after a synthesized one stays after it", () => {
    const core = new ProbeCore();
    expect(feed(freshState(), core, "\x1b[c then \x1b[6n done")).toBe(DA + "\x1b[1;1R");
  });

  test("a native reply before a synthesized one stays before it", () => {
    const core = new ProbeCore();
    expect(feed(freshState(), core, "\x1b[6n then \x1b[c done")).toBe("\x1b[1;1R" + DA);
  });

  test("several native probes in one chunk all reach the pty", () => {
    // The lane drains getResponse() until null; reading it once delivers one of
    // two and leaves the other to surface against a LATER chunk's probes.
    const core = new ProbeCore();
    expect(feed(freshState(), core, "a\x1b[6nb\x1b[6nc")).toBe("\x1b[1;1R\x1b[2;1R");
  });

  test("a bounded core FIFO survives a write larger than one core chunk", () => {
    // Probes at both ends of a >8192-byte write with room for ONE queued reply:
    // both arrive only because the lane drains inside the afterChunk hook.
    const core = new ProbeCore(1);
    const chunk = `\x1b[6n${"x".repeat(CORE_WRITE_CHUNK)}\x1b[6n`;
    expect(feed(freshState(), core, chunk)).toBe("\x1b[1;1R\x1b[2;1R");
  });

  test("a probe-free chunk is written to the core unsegmented", () => {
    const core = new ProbeCore();
    feed(freshState(), core, "plain output\r\n");
    expect(core.writes).toEqual(["plain output\r\n"]);
  });
});

describe("cross-chunk carry", () => {
  const PAYLOAD = "hi\x1b[cmid\x1b[6nend\x1b[>0qtail\x1b[0c!";
  const OWED = DA + "\x1b[1;1R" + XTV + DA;

  test("a multi-probe chunk split at EVERY byte boundary owes the same replies", () => {
    expect(feed(freshState(), new ProbeCore(), PAYLOAD)).toBe(OWED);
    for (let cut = 0; cut <= PAYLOAD.length; cut++) {
      const core = new ProbeCore();
      const state = freshState();
      const first = feed(state, core, PAYLOAD.slice(0, cut));
      const second = feed(state, core, PAYLOAD.slice(cut));
      expect({ cut, reply: first + second }).toEqual({ cut, reply: OWED });
      // Segmentation may not drop, duplicate or reorder a single byte.
      expect({ cut, seen: core.writes.join("") }).toEqual({ cut, seen: PAYLOAD });
      expect({ cut, carry: state.query_carry.length }).toEqual({ cut, carry: 0 });
    }
  });

  test("a probe split byte-by-byte across five chunks is answered exactly once", () => {
    const core = new ProbeCore();
    const state = freshState();
    const replies = [..."\x1b[>0q"].map((byte) => feed(state, core, byte));
    expect(replies).toEqual(["", "", "", "", XTV]);
  });

  test("the carry stays under its cap and abandons an endless CSI once", () => {
    const core = new ProbeCore();
    const state = freshState();
    let drops = 0;
    let maxCarry = 0;
    // A hostile stream that opens a CSI and keeps feeding parameter bytes.
    for (let i = 0; i < 200; i++) {
      const chunk = i === 0 ? "\x1b[" : "0;";
      drops += answerQueries(state, core, enc(chunk)).droppedCarry > 0 ? 1 : 0;
      maxCarry = Math.max(maxCarry, state.query_carry.length);
    }
    expect(maxCarry).toBeLessThanOrEqual(QUERY_CARRY_MAX);
    expect(drops).toBe(1);
    // The abandoned partial holds no ESC, so nothing false-matches after it and
    // the next real probe is still answered.
    expect(feed(state, core, "0;0c\x1b[c")).toBe(DA);
  });

  test("one session's partial probe never completes on another session's chunk", () => {
    const a = { state: freshState(), core: new ProbeCore() };
    const b = { state: freshState(), core: new ProbeCore() };
    expect(feed(a.state, a.core, "left\x1b[")).toBe("");
    expect(feed(b.state, b.core, "c right")).toBe("");
    expect(feed(a.state, a.core, "c")).toBe(DA);
  });

  test("the capture lane carries the tokenizer without answering or parsing", () => {
    // A frozen core parses nothing; the captured probes are answered once, FIFO,
    // by the post-boundary tail replay. What must NOT happen is the pre-capture
    // partial gluing onto the first post-rebuild chunk.
    const core = new ProbeCore();
    const state = freshState();
    expect(feed(state, core, "live\x1b[")).toBe("");
    expect(answerQueries(state, null, enc("c\x1b[c")).bytes).toBe("");
    expect(core.writes.join("")).toBe("live\x1b[");
    expect(feed(state, core, "\x1b[c")).toBe(DA);
  });
});

// ---------------------------------------------------------------------------
// Live wiring: appendScrollback -> answerQueries -> one batched keeper write
// ---------------------------------------------------------------------------

const CID = 41;
let keeper: FakeKeeper | null = null;

afterEach(() => {
  keeper?.restore();
  keeper = null;
});

/** `sessions` is SessionManager-internal: a reply-lane test injects one record
 *  directly rather than standing up a keeper-backed pty. */
function sessionsOf(mgr: SessionManager): Map<number, SessionRecord> {
  const internals = mgr as unknown as { sessions: Map<number, SessionRecord> };
  return internals.sessions;
}

async function liveSession(): Promise<SessionManager> {
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
  const wtermCore = await WasmBridge.load();
  wtermCore.init(80, 24);
  sessionsOf(mgr).set(CID, {
    sessionId: asSessionId("00000000-0000-0000-0000-0000000000aa"),
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell",
    cwd: "/",
    shellSpec: {} as never,
    fsm: {} as never,
    scrollback: createSbRing(),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    session_trace_id: "test-trace",
    cell_emit: initCellEmitState("test-grid"),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
    spawnedAtMs: Date.now(),
  });
  return mgr;
}

/** Every reply batch the worker put on the keeper socket, in order. */
function replies(): string[] {
  return (keeper?.writes ?? [])
    .filter((w) => w.type === MuxFrameType.PtyIn)
    .map((w) => dec.decode(w.bytes ?? new Uint8Array(0)));
}

const append = (mgr: SessionManager, chunk: string): number =>
  mgr.appendScrollback(CID, Buffer.from(chunk, "binary"));

describe("appendScrollback reply wiring", () => {
  test("one chunk's replies reach the keeper as ONE batch in probe order", async () => {
    const mgr = await liveSession();
    keeper = installFakeKeeper();
    append(mgr, "\x1b[c then \x1b[6n done\r\n");
    expect(replies().length).toBe(1);
    expect(replies()[0]).toMatch(/^\x1b\[\?1;2c\x1b\[\d+;\d+R$/);
  });

  test("two cursor reports in one chunk both reach the keeper", async () => {
    const mgr = await liveSession();
    keeper = installFakeKeeper();
    append(mgr, "a\x1b[6nbb\x1b[6nccc\r\n");
    expect(replies().length).toBe(1);
    expect(replies()[0]).toMatch(/^\x1b\[\d+;\d+R\x1b\[\d+;\d+R$/);
  });

  test("a Primary DA split across two pty chunks is answered once, on the second", async () => {
    const mgr = await liveSession();
    keeper = installFakeKeeper();
    append(mgr, "probe \x1b[");
    expect(replies()).toEqual([]);
    append(mgr, "c split across the boundary\r\n");
    expect(replies()).toEqual([DA]);
  });

  test("the carry lives on the session record and clears once the probe lands", async () => {
    const mgr = await liveSession();
    keeper = installFakeKeeper();
    const rec = sessionsOf(mgr).get(CID)!;
    append(mgr, "probe \x1b[>0");
    expect(dec.decode(rec.query_carry)).toBe("\x1b[>0");
    append(mgr, "q");
    expect(rec.query_carry.length).toBe(0);
    expect(replies()).toEqual([XTV]);
  });

  test("a probe split across two CAPTURED chunks is answered by the tail replay", async () => {
    // The capture freezes the core, so these bytes are parsed for the first time
    // by the post-boundary tail replay — where the two halves are contiguous.
    // The live carry advanced across them all along, so the session resumes
    // aligned instead of holding a partial that never completed.
    const mgr = await liveSession();
    keeper = installFakeKeeper();
    const rec = sessionsOf(mgr).get(CID)!;
    append(mgr, "before the resize\r\n");
    const capture = installResizeCapture(mgr, CID, "test");
    markResizeBoundary(mgr, CID, capture);
    mgr.appendCapturedScrollback(CID, Buffer.from("probe \x1b[", "binary"));
    mgr.appendCapturedScrollback(CID, Buffer.from("c captured tail\r\n", "binary"));
    expect(replies()).toEqual([]);
    expect(await rebuildTerminalCore(mgr, CID, 80, 24, capture)).toBe(true);
    await keeper.waitForWrite(MuxFrameType.PtyIn);
    expect(replies()).toEqual([DA]);
    expect(capture.forwardedReplies).toBe(1);
    expect(rec.query_carry.length).toBe(0);
  });
});
