// Double-layer parity: two independent wterm-core instances fed the same
// input bytes MUST produce identical cell grids. The cell-grid probe
// (gridToCellFrame → extracted text) replaces the old byte-path serialize/
// deserialize round-trip, which died with the byte path (cell-phase-4).
//
// Author 2026-06-17: "are you emulating the double layer because we
// have the wterm-core then the other terminal and all that, to ensure
// nothing gets fucked along the way."
//
// What this DOESN'T cover: the DOM renderer + CSS layer (@wterm/dom
// row spans, .term-scrollback-row CSS, the .wterm overflow rule from
// CLAUDE.md L11 row "no scroll bar"). Humanchrome two-tab smoke is
// the right harness for that — added next.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { WasmBridge } from "@wterm/core";
import { gridToCellFrame } from "@roost/shared/cell";
import type { TerminalCore } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

const SID = asSessionId("00000000-0000-0000-0000-000000000000");
const CID = 1;

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
}

async function injectSession(mgr: SessionManager, bytes: Uint8Array, cols: number, rows: number): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(cols, rows);
  wtermCore.writeRaw(bytes);
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(CID, {
    sessionId: SID,
    channelId: asChannelId(CID),
    socketPath: "/dev/null",
    kind: "shell" as const,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(new Uint8Array(bytes)),
    head_seq: bytes.length,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState(),
  });
}

/** Extract plain text from a wterm core via the cell-grid probe. */
function cellGridText(core: TerminalCore): string {
  const frame = gridToCellFrame(core, 0);
  const lines: string[] = [];
  for (const r of frame.scrollbackRows) lines.push(r.spans.map(s => s.text).join(""));
  for (const r of frame.viewportRows) lines.push(r.spans.map(s => s.text).join(""));
  return lines.join("\n");
}

/** Create a fresh client wterm, write the seed bytes, return its core. */
async function makeClientCore(seed: Uint8Array, cols: number, rows: number): Promise<TerminalCore> {
  const c = await WasmBridge.load();
  c.init(cols, rows);
  c.writeRaw(seed);
  return c;
}

describe("server↔client wterm-core parity (cell-grid round-trip)", () => {
  test("P1 — plain text produces identical cell grids", async () => {
    const mgr = freshMgr();
    const seed = new TextEncoder().encode("hello world\r\nsecond line\r\nthird\r\n");
    await injectSession(mgr, seed, 80, 24);
    const serverCore = mgr.shellByChannel(CID)!.wtermCore;
    const clientCore = await makeClientCore(seed, 80, 24);
    expect(cellGridText(clientCore)).toBe(cellGridText(serverCore));
  });

  test("P2 — SGR colors produce identical cell grids", async () => {
    const mgr = freshMgr();
    const seed = new TextEncoder().encode("\x1b[1;31;104mWARN\x1b[0m normal\r\n");
    await injectSession(mgr, seed, 40, 8);
    const serverCore = mgr.shellByChannel(CID)!.wtermCore;
    const clientCore = await makeClientCore(seed, 40, 8);
    expect(cellGridText(clientCore)).toBe(cellGridText(serverCore));
  });

  test("P3 — alt-screen enter/content/exit produces identical cell grids", async () => {
    const mgr = freshMgr();
    const seed = new TextEncoder().encode(
      "before\r\n\x1b[?1049hALT-SCREEN-PAINT\x1b[?1049lafter\r\n"
    );
    await injectSession(mgr, seed, 60, 12);
    const serverCore = mgr.shellByChannel(CID)!.wtermCore;
    const clientCore = await makeClientCore(seed, 60, 12);
    expect(cellGridText(clientCore)).toBe(cellGridText(serverCore));
  });

  test("P4 — cursor positioning (CUP) lands identically on both sides", async () => {
    const mgr = freshMgr();
    const seed = new TextEncoder().encode("\x1b[5;10HX\x1b[Horigin");
    await injectSession(mgr, seed, 40, 12);
    const serverCore = mgr.shellByChannel(CID)!.wtermCore;
    const clientCore = await makeClientCore(seed, 40, 12);
    expect(cellGridText(clientCore)).toBe(cellGridText(serverCore));
  });

  test("P5 — UTF-8 multi-byte (emoji, CJK) produces identical cell grids", async () => {
    const mgr = freshMgr();
    const seed = new TextEncoder().encode("héllo 🐙 中文 ñ\r\n");
    await injectSession(mgr, seed, 40, 8);
    const serverCore = mgr.shellByChannel(CID)!.wtermCore;
    const clientCore = await makeClientCore(seed, 40, 8);
    expect(cellGridText(clientCore)).toBe(cellGridText(serverCore));
  });

  test("P6 — two independent client cores fed the same bytes produce identical grids (browser-A vs browser-B parity)", async () => {
    const seed = new TextEncoder().encode(
      "row1\r\nrow2 with \x1b[33myellow\x1b[0m\r\nrow3 \x1b[7minverse\x1b[0m\r\n"
    );
    const clientA = await makeClientCore(seed, 50, 10);
    const clientB = await makeClientCore(seed, 50, 10);
    expect(cellGridText(clientA)).toBe(cellGridText(clientB));
  });
});
