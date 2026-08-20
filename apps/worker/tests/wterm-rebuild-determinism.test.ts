// OPT2-1 guard: wtermCore resize is a deterministic rebuild from the raw
// ring, NOT @wterm/core's path-dependent in-place resize. The bug it fixes:
// "phone rotation sometimes mangles history" — the same final cols×rows
// produced a different grid depending on the resize path taken to get there
// (shrink pushes rows to scrollback, grow appends blanks, never reverses).
// A correct rebuild is a pure function of (ring, cols, rows): A→B→A returns
// byte-identical output, and a managed rebuild equals an independent fresh
// replay. If someone reverts _recomputeViewport to rec.wtermCore.resize(),
// the A→B→A test fails.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared/wire";
import { gridToCellFrame } from "@roost/shared/cell";
import { WasmBridge } from "@wterm/core";
import type { TerminalCore } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import { installResizeCapture, rebuildTerminalCore } from "../src/session-resize-capture.ts";
import type { ResizeCapture } from "../src/session-resize-capture.ts";
import {
  MAX_SEQ_LOOKBEHIND, rewindToSequenceStart, skipOrphanSequencePrefix,
} from "../src/terminal-replay-align.ts";

function cellGridText(core: TerminalCore): string {
  const frame = gridToCellFrame(core, 0, "test-grid:0");
  const lines: string[] = [];
  for (const r of frame.scrollbackRows) lines.push(r.spans.map(s => s.text).join(""));
  for (const r of frame.viewportRows) lines.push(r.spans.map(s => s.text).join(""));
  return lines.join("\n");
}

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
}

async function injectSession(
  mgr: SessionManager, channelId: number, bytes: string, cols: number, rows: number,
  opts: { altMode?: boolean; headSeq?: number } = {},
): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(cols, rows);
  wtermCore.writeRaw(new TextEncoder().encode(bytes));
  const record = {
    sessionId: asSessionId("00000000-0000-0000-0000-000000000000"),
    channelId: asChannelId(channelId),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(new TextEncoder().encode(bytes)),
    head_seq: opts.headSeq ?? bytes.length,
    alt_mode: opts.altMode ?? false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState("test-grid"),
  };
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, record);
}

const rebuild = (mgr: SessionManager, ch: number, c: number, r: number): Promise<void> =>
  (mgr as unknown as { _rebuildWtermCore: (ch: number, c: number, r: number) => Promise<void> })
    ._rebuildWtermCore(ch, c, r);

const coreOf = (mgr: SessionManager, ch: number) =>
  (mgr as unknown as { sessions: Map<number, { wtermCore: import("@wterm/core").TerminalCore }> })
    .sessions.get(ch)!.wtermCore;

// 60 numbered wide lines → a shrink pushes many rows into scrollback, so an
// asymmetric in-place resize would visibly drift; a rebuild must not.
const CONTENT = Array.from({ length: 60 }, (_, i) => `line-${i} ${"x".repeat(40)}`).join("\r\n") + "\r\n";

describe("OPT2-1 wtermCore rebuild determinism", () => {
  test("A→B→A yields a cell-grid-identical grid (path-independent)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, CONTENT, 100, 30);
    await rebuild(mgr, 1, 100, 30);
    const gridA1 = cellGridText(coreOf(mgr, 1));
    await rebuild(mgr, 1, 40, 10);   // shrink (dumps rows to scrollback)
    await rebuild(mgr, 1, 120, 50);  // grow
    await rebuild(mgr, 1, 100, 30);  // back to A
    const gridA2 = cellGridText(coreOf(mgr, 1));
    expect(gridA2).toBe(gridA1);
  });

  test("managed rebuild equals an independent fresh replay (pure function)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 2, CONTENT, 100, 40);
    await rebuild(mgr, 2, 56, 18);
    const managed = cellGridText(coreOf(mgr, 2));

    const ref = await WasmBridge.load();
    ref.init(56, 18);
    ref.writeRaw(new TextEncoder().encode(CONTENT));
    expect(managed).toBe(cellGridText(ref));
  });

  // Direct symptom tripwire for the memory's literal failure: repeated HEIGHT
  // wobble (mobile keyboard / window drag / orientation) drifting the
  // scrollback line count "99→132→165→203" and skewing the grid. @wterm/core's
  // in-place row resize is asymmetric (shrink dumps rows to scrollback, grow
  // appends blanks, never pulls them back) so an in-place path creeps the
  // count every cycle; the rebuild is a pure function of (ring, cols, rows) so
  // every return to the start size is byte- AND count-identical, forever.
  // Reverting _recomputeViewport to rec.wtermCore.resize() fails this on the
  // first oscillation. See project_scrollback_raw_ring_single_source / L11.
  test("sustained resize storm holds scrollback COUNT + grid flat (no oscillation creep)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 3, CONTENT, 100, 30);
    await rebuild(mgr, 3, 100, 30);
    const baseCount = coreOf(mgr, 3).getScrollbackCount();
    const baseGrid = cellGridText(coreOf(mgr, 3));
    expect(baseCount).toBeGreaterThan(0); // content must overflow into scrollback

    for (let cycle = 0; cycle < 20; cycle++) {
      await rebuild(mgr, 3, 100, 12); // shrink height (would dump to scrollback in-place)
      await rebuild(mgr, 3, 100, 30); // restore (in-place would append blanks, never reverse)
    }

    expect(coreOf(mgr, 3).getScrollbackCount()).toBe(baseCount);
    expect(cellGridText(coreOf(mgr, 3))).toBe(baseGrid);
  });
});

// L11 "htop shows `32m1969M` at row 0 after a resize, with permanently stale
// regions behind it". The resize boundary is `head_seq` at the keeper's result
// frame and head_seq advances by WHOLE PTY CHUNKS, so it lands wherever the pty
// flushed — including between `ESC [` and `32m`. The rebuilt core's parser is
// COLD, so a tail starting mid-sequence prints the remainder as literal text,
// and on alt-screen that text never goes away: the TUI's differential redraw
// only rewrites cells it believes are stale. terminal-replay-align.ts rewinds
// the boundary onto the sequence it split; these tests are the guard.
describe("replay parser alignment (terminal-replay-align)", () => {
  const enc = new TextEncoder();

  test("rewinds a boundary that splits ESC[32m, at either cut", () => {
    const bytes = enc.encode("abc\x1b[32m"); // ESC at 3, '[' at 4, final 'm' at 7
    expect(rewindToSequenceStart(bytes, 5)).toBe(3); // cut after `ESC [`
    expect(rewindToSequenceStart(bytes, 6)).toBe(3); // cut after `ESC [ 3`
    expect(rewindToSequenceStart(bytes, 7)).toBe(3); // cut before the final
  });

  test("leaves a boundary already at a token boundary untouched", () => {
    const bytes = enc.encode("abc\x1b[32m");
    expect(rewindToSequenceStart(bytes, 8)).toBe(8); // just past the final `m`
    expect(rewindToSequenceStart(bytes, 3)).toBe(3); // on the ESC itself
    expect(rewindToSequenceStart(bytes, 0)).toBe(0);
    expect(rewindToSequenceStart(enc.encode(""), 0)).toBe(0);
  });

  test("never rewinds past a TERMINATED sequence", () => {
    // The completed `ESC [ 3 2 m` must not drag the replay back over text the
    // head already painted — that would double it.
    expect(rewindToSequenceStart(enc.encode("\x1b[32mhello"), 10)).toBe(10);
    expect(rewindToSequenceStart(enc.encode("\x1b]0;title\x07more"), 13)).toBe(13); // OSC + BEL
    expect(rewindToSequenceStart(enc.encode("\x1bMabc"), 5)).toBe(5); // 2-byte ESC M
    expect(rewindToSequenceStart(enc.encode("\x1b(Babc"), 6)).toBe(6); // nF ESC ( B
  });

  test("rewinds every unterminated introducer form", () => {
    expect(rewindToSequenceStart(enc.encode("x\x1b"), 2)).toBe(1); // bare trailing ESC
    expect(rewindToSequenceStart(enc.encode("\x1b]0;titl"), 8)).toBe(0); // OSC, no BEL yet
    expect(rewindToSequenceStart(enc.encode("\x1b(B"), 2)).toBe(0); // nF, final not in yet
    expect(rewindToSequenceStart(enc.encode("\x1b\\"), 1)).toBe(0); // ST split from its ESC
  });

  test("look-behind is bounded: an unterminated CSI past the cap returns start", () => {
    // `1;` repeats are CSI parameter bytes, so the sequence is genuinely open the
    // whole way — only the cap can stop the scan.
    const near = enc.encode("\x1b[" + "1;".repeat(50));
    expect(rewindToSequenceStart(near, near.length)).toBe(0);
    const far = enc.encode("\x1b[" + "1;".repeat(MAX_SEQ_LOOKBEHIND));
    expect(far.length).toBeGreaterThan(MAX_SEQ_LOOKBEHIND);
    expect(rewindToSequenceStart(far, far.length)).toBe(far.length);
  });

  test("never returns an index above start", () => {
    const bytes = enc.encode("\x1b[32mplain\x1b[0m tail\x1b[");
    for (let start = 0; start <= bytes.length; start++) {
      expect(rewindToSequenceStart(bytes, start)).toBeLessThanOrEqual(start);
    }
  });

  test("skipOrphanSequencePrefix lands on the first nearby ESC", () => {
    expect(skipOrphanSequencePrefix(enc.encode("2m1969M\x1b[32m"))).toBe(7);
    expect(skipOrphanSequencePrefix(enc.encode("\x1b[32m"))).toBe(0);
    expect(skipOrphanSequencePrefix(enc.encode(""))).toBe(0);
  });

  // An eviction cut that lands in TEXT has no orphan sequence to skip, so the
  // whole window is replayable. Scanning to the first ESC regardless would
  // discard every retained byte of an ESC-free window (a build log, a `cat` of
  // a big file) — a total history loss in place of a 256-byte repair.
  test("keeps an ESC-free window instead of discarding it", () => {
    expect(skipOrphanSequencePrefix(enc.encode("no escapes here"))).toBe(0);
    const far = enc.encode(`${"plain text ".repeat(40)}\x1b[32m`);
    expect(far.indexOf(0x1b)).toBeGreaterThan(MAX_SEQ_LOOKBEHIND);
    expect(skipOrphanSequencePrefix(far)).toBe(0);
  });

  // An eviction cut splits a multi-byte character far more often than it splits
  // a sequence: a continuation byte can never start a codepoint, so replaying it
  // renders U+FFFD.
  test("drops a leading split UTF-8 codepoint", () => {
    const split = enc.encode("héllo").subarray(2); // tail of é's 2-byte form
    expect(split[0]! & 0xc0).toBe(0x80);
    expect(skipOrphanSequencePrefix(split)).toBe(1);
    const both = new Uint8Array([0xa9, 0x1b, 0x5b, 0x33, 0x32, 0x6d]);
    expect(skipOrphanSequencePrefix(both)).toBe(1);
  });
});

describe("rebuildTerminalCore replays across a split sequence", () => {
  const captureAt = (mgr: SessionManager, ch: number, seq: number, alt: boolean): ResizeCapture => {
    const capture = installResizeCapture(mgr, ch, "test-align");
    capture.boundarySeq = seq;
    capture.boundaryAltMode = alt;
    return capture;
  };

  // The exact production shape: htop in alt-screen, the pty flushing a chunk
  // that ends between `ESC [` and `32m`, and the keeper's resize boundary
  // landing on that flush. The alt path deliberately does NOT replay the head,
  // so before the alignment fix the fresh core met `32m1969M` with a cold parser
  // and printed it.
  test("alt-screen boundary splitting ESC[32m does not render literal `32m`", async () => {
    const mgr = freshMgr();
    const pre = "\x1b[?1049h\x1b[2J\x1b[H" + "old geometry paint\r\n" + "\x1b[";
    const post = "32m1969M free\x1b[0m";
    await injectSession(mgr, 10, pre + post, 80, 24, { altMode: true });
    const capture = captureAt(mgr, 10, pre.length, true);

    expect(await rebuildTerminalCore(mgr, 10, 100, 30, capture)).toBe(true);
    const grid = cellGridText(coreOf(mgr, 10));
    expect(grid).not.toContain("32m");
    expect(grid).toContain("1969M free");
  });

  // Same defect on the non-alt path, where the head IS replayed: the rewound
  // lead-in must be written exactly ONCE (as the prefix), never left in the head
  // as well, or the sequence is fed twice.
  test("non-alt boundary splitting ESC[32m replays the lead-in exactly once", async () => {
    const mgr = freshMgr();
    const pre = "before-";
    const split = "\x1b[";
    const post = "32mGREEN\x1b[0m";
    await injectSession(mgr, 11, pre + split + post, 80, 24);
    const capture = captureAt(mgr, 11, (pre + split).length, false);

    expect(await rebuildTerminalCore(mgr, 11, 100, 30, capture)).toBe(true);
    const grid = cellGridText(coreOf(mgr, 11));
    expect(grid).not.toContain("32m");
    expect(grid).toContain("before-GREEN");
  });

  // Ring eviction below the boundary: the clamp puts the replay start at the
  // ring's oldest retained byte, which is an arbitrary eviction offset. There is
  // nothing behind it to rewind onto, so the leading remnant is skipped forward
  // to the first byte with knowable parser state.
  test("evicted boundary skips the orphan remnant instead of printing it", async () => {
    const mgr = freshMgr();
    const retained = "2m1969M\x1b[32mGREEN\x1b[0m"; // ring cut mid-`ESC[32m`
    // head_seq far ahead of the retained window => retainedStart > boundarySeq.
    await injectSession(mgr, 12, retained, 80, 24, { headSeq: retained.length + 1000 });
    const capture = captureAt(mgr, 12, 500, false);

    expect(await rebuildTerminalCore(mgr, 12, 100, 30, capture)).toBe(true);
    expect(capture.ringEvicted).toBe(true);
    const grid = cellGridText(coreOf(mgr, 12));
    expect(grid).not.toContain("2m1969M");
    expect(grid).toContain("GREEN");
  });
});
