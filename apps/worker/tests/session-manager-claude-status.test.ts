// herdr agent-status wiring: SessionManager scrapes each claude session's
// wtermCore grid off the PTY byte path (emitUpstreamChunk → _scheduleDetect →
// _runDetect), arbitrates screen + byte-activity into a stable status, and emits
// it upstream on CHANGE via sendClaudeStatusUpstream. This drives the REAL byte
// path through a REAL wterm core (no fake grid) and asserts the emitted status —
// the producer half of worker→WClaudeStatus→coord→claudeStatusBus→SPA chip.
//
// Working = braille spinner in the OSC title (claude manifest osc_title_working).
// Idle    = the `❯` prompt box in the grid, once the byte stream goes quiet
//           (arbiter holds working→idle until AGENT_WORKING_GRACE_MS of silence).

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { initCellEmitState } from "@roost/shared/cell";
import { WasmBridge } from "@wterm/core";

function mgrWithStatusLog(): { mgr: SessionManager; statuses: string[] } {
  const statuses: string[] = [];
  const mgr = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    hookSocketPath: "/dev/null",
    sendBinaryUpstream: () => {},
    sendCellGridUpstream: () => {},
    sendClaudeStatusUpstream: (_ch, status) => { statuses.push(status); },
  });
  return { mgr, statuses };
}

async function injectSession(mgr: SessionManager, channelId: number, kind: "claude" | "shell"): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(80, 24);
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, {
    sessionId: asSessionId("00000000-0000-0000-0000-000000000000"),
    channelId: asChannelId(channelId), socketPath: "/dev/null", kind, cwd: "/",
    fsm: {} as never, bridge: null, scrollback: new Uint8Array(0), head_seq: 0,
    alt_mode: false, mode_carry: new Uint8Array(0), osc7_carry: new Uint8Array(0),
    wtermCore, cell_emit: initCellEmitState(),
  });
}

// UTF-8 (not "binary") so multi-byte braille survives into the wterm core.
const feed = (mgr: SessionManager, ch: number, s: string): void =>
  (mgr as unknown as { emitUpstreamChunk(c: number, b: Buffer): void }).emitUpstreamChunk(ch, Buffer.from(s, "utf8"));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

const RULE = "─".repeat(25);
const setTitle = (t: string): string => `\x1b]0;${t}\x07`;

describe("herdr claude-status wiring (SessionManager)", () => {
  test("braille-spinner title → running emitted", async () => {
    const { mgr, statuses } = mgrWithStatusLog();
    await injectSession(mgr, 1, "claude");
    feed(mgr, 1, `${setTitle("⠹ Building the thing")}working body\r\n`);
    await sleep(220); // > DETECT_DEBOUNCE_MS (150)
    expect(statuses.at(-1)).toBe("running");
  });

  test("quiet ❯ prompt box → idle after the working→idle hold", async () => {
    const { mgr, statuses } = mgrWithStatusLog();
    await injectSession(mgr, 1, "claude");
    feed(mgr, 1, `${setTitle("⠹ Building")}work\r\n`);
    await sleep(220);
    expect(statuses.at(-1)).toBe("running");
    // Clear the spinner title + render the idle prompt box; then stay quiet.
    feed(mgr, 1, `${setTitle("claude")}\x1b[2J\x1b[H⏺ Done.\r\n\r\n${RULE}\r\n ❯ type your message\r\n${RULE}\r\n  ? for shortcuts\r\n`);
    await sleep(3400); // > AGENT_WORKING_GRACE_MS (3000) so the held idle commits
    expect(statuses.at(-1)).toBe("idle");
  });

  // The load-bearing case: users run claude INSIDE a shell session, so detection
  // must be kind-agnostic (the manifest is the filter, not the pane kind).
  test("claude running inside a kind:shell session still emits (kind-agnostic)", async () => {
    const { mgr, statuses } = mgrWithStatusLog();
    await injectSession(mgr, 1, "shell");
    feed(mgr, 1, `${setTitle("⠹ Building the thing")}work\r\n`);
    await sleep(220);
    expect(statuses.at(-1)).toBe("running");
  });

  // resume-seed / idle-sweep path: a session whose grid already holds an idle
  // claude screen (as resume() reconstructs from the ring) must surface "idle"
  // WITHOUT any byte-path activity — the fix for idle-across-worker-restart.
  test("idle grid with no recent bytes → idle (resume-seed path)", async () => {
    const { mgr, statuses } = mgrWithStatusLog();
    await injectSession(mgr, 1, "shell");
    const rec = (mgr as unknown as { sessions: Map<number, { wtermCore: { writeRaw(b: Uint8Array): void } }> }).sessions.get(1)!;
    rec.wtermCore.writeRaw(new TextEncoder().encode(`⏺ Done.\r\n\r\n${RULE}\r\n ❯ type your message\r\n${RULE}\r\n  ? for shortcuts\r\n`));
    (mgr as unknown as { _runDetect(c: number): void })._runDetect(1);
    await sleep(50);
    expect(statuses.at(-1)).toBe("idle");
  });

  test("plain shell (no agent UI) emits nothing", async () => {
    const { mgr, statuses } = mgrWithStatusLog();
    await injectSession(mgr, 1, "shell");
    feed(mgr, 1, `${setTitle("~/code — zsh")}$ ls -la\r\ntotal 8\r\nfile.txt\r\n`);
    await sleep(220);
    expect(statuses.length).toBe(0);
  });
});
