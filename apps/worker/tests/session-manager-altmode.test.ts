// phase-ssb-altmode: regression test for alt-screen tracking + snapshot
// prefix. Diagnosed 2026-06-14 from user "wallpaper of stale text +
// disconnected live text" symptom: keeper's 8MB ring evicted the
// original ESC[?1049h enter sequence, snapshot replayed in main-screen
// mode, claude UI redraws polluted scrollback, live cursor-positioned
// bytes hit wrong rows. Workaround: scan chunks for DEC 1049/47/1047
// transitions, prepend ESC[?1049h when serving snapshots if currently
// alt-screen.

import { describe, test, expect } from "bun:test";
import { SessionManager } from "../src/session-manager.ts";
import { asSessionId, asChannelId, asWorkerFp } from "@roost/shared";
import { WasmBridge } from "@wterm/core";
import { initCellEmitState } from "@roost/shared/cell";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
  });
}

type InternalSession = { alt_mode: boolean };

async function injectSession(mgr: SessionManager, channelId: number, bytes: string, headSeq: number, altMode = false, kind: "shell" | "claude" = "shell"): Promise<void> {
  const wtermCore = await WasmBridge.load();
  wtermCore.init(80, 24);
  const record = {
    sessionId: asSessionId("00000000-0000-0000-0000-000000000000"),
    channelId: asChannelId(channelId),
    socketPath: "/dev/null",
    kind,
    cwd: "/",
    fsm: {} as never,
    bridge: null,
    scrollback: createSbRing(new TextEncoder().encode(bytes)),
    head_seq: headSeq,
    alt_mode: altMode,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore,
    cell_emit: initCellEmitState(),
  };
  (mgr as unknown as { sessions: Map<number, unknown> }).sessions.set(channelId, record);
}

function callAppend(mgr: SessionManager, channelId: number, chunkStr: string): number {
  const chunk = Buffer.from(chunkStr, "binary");
  return (mgr as unknown as { appendScrollback: (c: number, b: Buffer) => number }).appendScrollback(channelId, chunk);
}

const ALT_ENTER_1049 = "\x1b[?1049h";
const ALT_EXIT_1049  = "\x1b[?1049l";
const ALT_ENTER_47   = "\x1b[?47h";
const ALT_EXIT_47    = "\x1b[?47l";
const ALT_ENTER_1047 = "\x1b[?1047h";
const ALT_EXIT_1047  = "\x1b[?1047l";

describe("alt-mode scanner — single-chunk transitions", () => {
  test("ESC[?1049h flips alt_mode true", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, "hello" + ALT_ENTER_1049 + "world");
    const rec = (mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!;
    expect(rec.alt_mode).toBe(true);
  });

  test("ESC[?1049l flips alt_mode false", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, true);
    callAppend(mgr, 1, "x" + ALT_EXIT_1049 + "y");
    const rec = (mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!;
    expect(rec.alt_mode).toBe(false);
  });

  test("ESC[?47h flips alt_mode true (legacy variant)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, ALT_ENTER_47);
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("ESC[?1047h flips alt_mode true (mid variant)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, ALT_ENTER_1047);
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("multiple transitions take the LAST one", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, ALT_ENTER_1049 + "x" + ALT_EXIT_1049 + "y" + ALT_ENTER_1049 + "z");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("no transition leaves alt_mode unchanged", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, true);
    callAppend(mgr, 1, "plain text no escapes");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });
});

// Alt-mode is STREAM-DRIVEN, never hardcoded from a program label. MAIN-screen
// output keeps scrollback; only a real escape sequence enters alt-screen.
describe("alt-mode is stream-driven", () => {
  test("main-screen output stays alt_mode=false and keeps scrollback", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, /*altMode*/ false);
    callAppend(mgr, 1, "Welcome\nHISTLINE-1\nHISTLINE-2\n");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(false);
  });

  test("only a real ESC[?1049h enables alt mode", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, "prose" + ALT_ENTER_1049 + "tui");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("a shell TUI (vim) also enters alt mode", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false, "shell");
    callAppend(mgr, 1, ALT_ENTER_1049);
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });
});

describe("alt-mode scanner — split across chunk boundaries (mode_carry)", () => {
  test("ESC[?1049h split between two chunks still triggers", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false);
    callAppend(mgr, 1, "filler\x1b[?10");
    callAppend(mgr, 1, "49h" + "post");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("ESC[?1049l split at the very last byte still triggers exit", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, true);
    callAppend(mgr, 1, "x\x1b[?1049");
    callAppend(mgr, 1, "l");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(false);
  });
});

