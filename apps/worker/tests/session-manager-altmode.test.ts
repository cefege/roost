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

function freshMgr(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    hookSocketPath: "/dev/null",
  });
}

type InternalSession = {
  scrollback: Uint8Array;
  head_seq: number;
  alt_mode: boolean;
  mode_carry: Uint8Array;
};

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
    scrollback: new TextEncoder().encode(bytes),
    head_seq: headSeq,
    alt_mode: altMode,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    wtermCore,
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

// Phase-1 (2026-06-22b): alt_mode is STREAM-DRIVEN, never hardcoded from kind.
// Guards the "claude has no scrollback in cell mode / lost on reload" bug —
// claude is MAIN-SCREEN (never emits ESC[?1049h) so it must stay alt_mode=false
// to keep scrollback. Re-hardcoding alt_mode:true for kind==="claude" anywhere
// (spawnClaude / respawn / resume record) re-breaks this.
describe("alt-mode is kind-AGNOSTIC (Phase-1 scrollback fix)", () => {
  test("kind:claude on main-screen output stays alt_mode=false (keeps scrollback)", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, /*altMode*/ false, /*kind*/ "claude");
    callAppend(mgr, 1, "Welcome to Claude Code\nHISTLINE-1\nHISTLINE-2\n");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(false);
  });

  test("kind:claude flips alt_mode=true ONLY on a real ESC[?1049h", async () => {
    const mgr = freshMgr();
    await injectSession(mgr, 1, "", 0, false, "claude");
    callAppend(mgr, 1, "prose" + ALT_ENTER_1049 + "tui");
    expect((mgr as unknown as { sessions: Map<number, InternalSession> }).sessions.get(1)!.alt_mode).toBe(true);
  });

  test("kind:shell running a TUI (vim) goes alt_mode=true — carve-out is not claude-only", async () => {
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

