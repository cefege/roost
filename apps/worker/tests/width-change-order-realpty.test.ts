// Reproduces + guards the user's actual bug: "every width change fucks up the
// order." Drives a REAL shell, produces ordered lines at ONE width, then
// changes the width and asserts the served history is STILL IN ORDER (and
// byte-identical across an A→B→A cycle). Under the cell-mode model (cell-phase-4),
// the grid is served via gridToCellFrame at the worker-rendered size — one
// consistent width → order preserved.
// no browser, no coord, no physical device.

import { gridToCellFrame } from "@roost/shared/cell";
import type { TerminalCore } from "@wterm/core";
import { describe, test, expect } from "bun:test";
import { tmpdir } from "node:os";
import { SessionManager } from "../src/session-manager.ts";
import { asWorkerFp } from "@roost/shared";
import { readRing, type SbRing } from "../src/session-scrollback-ring.ts";

const FP = "ab".repeat(32);

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => {},
  });
}
function cellGridText(core: TerminalCore): string {
  const frame = gridToCellFrame(core, 0);
  const lines: string[] = [];
  for (const r of frame.scrollbackRows) lines.push(r.spans.map(s => s.text).join(""));
  for (const r of frame.viewportRows) lines.push(r.spans.map(s => s.text).join(""));
  return lines.join("\n");
}
type Internal = {
  _wtermRebuildChain: Map<number, Promise<void>>;
  _recomputeViewport: (ch: number) => void;
  sessions: Map<number, { scrollback: SbRing }>;
};
const asInternal = (m: SessionManager) => m as unknown as Internal;

async function waitForRing(m: SessionManager, ch: number, needle: string, timeoutMs: number): Promise<boolean> {
  const dec = new TextDecoder();
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (dec.decode(readRing(asInternal(m).sessions.get(ch)!.scrollback)).includes(needle)) return true;
    // Real PTY integration exposes no output event; this is a bounded
    // readiness poll against the actual keeper/PTY transport.
    await Bun.sleep(120);
  }
  return false;
}
async function resizeTo(m: SessionManager, ch: number, cols: number, rows: number): Promise<void> {
  m.claimViewport(ch, FP, cols, rows);
  asInternal(m)._recomputeViewport(ch);
  await asInternal(m)._wtermRebuildChain.get(ch);
}
async function servedHistory(m: SessionManager, ch: number): Promise<string> {
  const core = m.shellByChannel(ch)!.wtermCore;
  return cellGridText(core);
}
function markerSequence(text: string): number[] {
  const plain = text.replace(/\x1b\[[0-9;?]*[A-Za-z]/g, "");
  const out: number[] = [];
  for (const mm of plain.matchAll(/L(\d+)Z/g)) out.push(Number(mm[1]));
  return out;
}
function isNonDecreasing(xs: number[]): boolean {
  for (let i = 1; i < xs.length; i++) if (xs[i] < xs[i - 1]) return false;
  return true;
}

describe("OPT2 deterministic-rebuild model: real shell width-change preserves order", () => {
  test("content made at width 80, viewed at 159 → history IN ORDER, stable A→B→A", async () => {
    const m = freshMgr();
    const rec = await m.spawnShell(tmpdir(), 80, 30);
    const ch = rec.channelId;

    await m.input(ch, new TextEncoder().encode("echo RDY$((6*7))\n"));
    expect(await waitForRing(m, ch, "RDY42", 8000)).toBe(true);

    // 150 ordered lines produced at width 80 (marker Lk Z is output-only).
    await m.input(
      ch,
      new TextEncoder().encode("for i in $(seq 1 150); do echo \"L${i}Z line $i\"; done\n"),
    );
    expect(await waitForRing(m, ch, "L150Z", 12000)).toBe(true);

    // The user's scenario: change the width (80 → 159) and read history.
    await resizeTo(m, ch, 159, 30);
    const at159 = await servedHistory(m, ch);
    const seq159 = markerSequence(at159);
    expect(seq159.length).toBeGreaterThan(20);          // history actually present
    expect(isNonDecreasing(seq159)).toBe(true);         // NOT reordered
    expect(seq159[0]).toBeLessThan(seq159[seq159.length - 1]); // oldest first
    expect(seq159[seq159.length - 1]).toBe(150);        // newest at the end (bottom)

    // Oscillate width and come back — must be identical + still ordered.
    await resizeTo(m, ch, 48, 12);
    await resizeTo(m, ch, 159, 30);
    const at159b = await servedHistory(m, ch);
    expect(at159b).toBe(at159);                         // deterministic
    expect(isNonDecreasing(markerSequence(at159b))).toBe(true);

    m.kill(ch);
    m.dispose();
  }, 40000);
});
