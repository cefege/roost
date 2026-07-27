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
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { gridToCellFrame } from "@roost/shared/cell";
import { WasmBridge } from "@wterm/core";
import type { TerminalCore } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";

function cellGridText(core: TerminalCore): string {
  const frame = gridToCellFrame(core, 0);
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
    scrollback: new TextEncoder().encode(bytes),
    head_seq: bytes.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore,
    cell_emit: initCellEmitState(),
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
