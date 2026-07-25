// Mirror-engine watcher contracts — the per-OSC-title entry point that turns a
// terminal omp session into a chat pane. The transcript-watcher module is
// mocked, so resolve outcome and transcript appends are scripted: no lsof, no
// fs.watch, no wall-clock waiting.
//
// What this pins, and why each would otherwise silently regress:
//   - re-entrancy: session-emit calls _ensureChatWatch on EVERY title emission
//     (~12.5/s on omp's Braille spinner) and the resolve is async, so without an
//     in-flight guard every tick starts another resolve + fs watcher on the same
//     file — ~100 concurrent `lsof -c bun` calls and a 40-frame duplicate burst
//     per transcript append;
//   - a failed resolve stays RETRYABLE: omp emits its first title before it
//     creates its transcript, so latching on the first miss would kill the pane
//     for the session's whole life — but bounded, or a session whose transcript
//     can never be resolved probes lsof forever;
//   - one transcript, one session: two sessions in the same cwd both resolved
//     the single open transcript and both mirrored it;
//   - run state rides the OSC title separator: `streaming` is the only turn
//     signal the mirror engine has, and a turn runs for minutes between appends,
//     so a state flip must publish a frame of its own.

import { test, expect, mock } from "bun:test";
import type { ChatFrame, ChatMessage } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../../src/session-record.ts";
import type { SessionManager } from "../../../src/session-manager.ts";

/** Scripted resolve outcome, swapped per test before the call under test. */
let nextPath: string | null = null;
let resolveCalls = 0;
/** Appenders handed out by the mocked tailer, keyed by transcript path. */
const tailers = new Map<string, (msgs: ChatMessage[], seq: number) => void>();
let disposed = 0;

await mock.module("../../../src/chat/omp/transcript-watcher.ts", () => ({
	resolveTranscriptPath: async () => {
		resolveCalls++;
		return nextPath ? { path: nextPath, via: "lsof" as const } : null;
	},
	startTranscriptWatcher: (path: string, onAppend: (m: ChatMessage[], s: number) => void) => {
		tailers.set(path, onAppend);
		return { dispose: () => { disposed++; tailers.delete(path); } };
	},
}));

// Dynamic by necessity: a static import is hoisted above the mock.module call,
// so session-chat would capture the REAL resolveTranscriptPath (a global
// `lsof -c bun`, 12 s per attempt) before the stub is installed.
const { _ensureChatWatch, _emitChatRunState, _disposeChatWatch } = await import("../../../src/session-chat.ts");

const IDLE = "\u03C0 > roost";
const WORKING = "\u03C0 \u280B roost";
const ATTENTION = "\u03C0 ! roost";

/** Let the resolve's promise chain run. The mocked resolver settles
 *  immediately, so the continuation is one microtask away — no timers. */
async function flush(): Promise<void> {
	for (let i = 0; i < 4; i++) await Promise.resolve();
}

interface Harness {
	mgr: SessionManager;
	rec: SessionRecord;
	frames: ChatFrame[];
}

/** A SessionManager stand-in holding only what the watcher touches. `sessions`
 *  is a real Map because the code under test calls .get() and .values(). */
function harness(channelId: number, extra: SessionRecord[] = []): Harness {
	const frames: ChatFrame[] = [];
	const rec = {
		sessionId: `sid-${channelId}`, channelId, cwd: "/w", chat_seq: 0, childPid: null,
	} as unknown as SessionRecord;
	const sessions = new Map<number, SessionRecord>([[channelId, rec]]);
	extra.forEach((e, i) => sessions.set(9000 + i, e));
	const mgr = {
		sessions,
		lastOscTitle: new Map<number, string>([[channelId, IDLE]]),
		sendChatFrameUpstream: (_ch: number, f: ChatFrame) => { frames.push(f); },
	} as unknown as SessionManager;
	return { mgr, rec, frames };
}

test("re-entrant title emissions start exactly one resolve and one tailer", async () => {
	resolveCalls = 0; nextPath = "/t/one.jsonl";
	const h = harness(1);
	// 40 emissions is ~3 s of omp's spinner at 12.5 Hz, all before the resolve settles.
	for (let i = 0; i < 40; i++) _ensureChatWatch.call(h.mgr, 1);
	expect(resolveCalls).toBe(1);
	await flush();
	expect(h.rec.chatWatchDispose).toBeDefined();
	// Still one after the watcher is up: the guard moves from "starting" to "running".
	for (let i = 0; i < 40; i++) _ensureChatWatch.call(h.mgr, 1);
	expect(resolveCalls).toBe(1);
	// Exactly one reset frame reached the client — the pane reseeds once, not 40×.
	expect(h.frames.filter((f) => f.reset)).toHaveLength(1);
	_disposeChatWatch(h.rec);
});

test("a transcript that does not exist yet is retried, not latched", async () => {
	resolveCalls = 0; nextPath = null;
	const h = harness(2);
	_ensureChatWatch.call(h.mgr, 2);
	await flush();
	expect(h.rec.chatWatchTries).toBe(1);
	expect(h.rec.chatWatchStarting).toBe(false);
	// A miss must not wipe the pane: no reset frame until a transcript is bound.
	expect(h.frames).toHaveLength(0);

	nextPath = "/t/late.jsonl";
	_ensureChatWatch.call(h.mgr, 2);
	await flush();
	expect(resolveCalls).toBe(2);
	expect(h.rec.chatWatchDispose).toBeDefined();
	_disposeChatWatch(h.rec);
});

test("an unresolvable transcript stops probing at the cap", async () => {
	resolveCalls = 0; nextPath = null;
	const h = harness(3);
	for (let i = 0; i < 30; i++) {
		_ensureChatWatch.call(h.mgr, 3);
		await flush();
	}
	expect(resolveCalls).toBe(8);
	expect(h.rec.chatWatchStarting).toBe(true);
	expect(h.rec.chatWatchDispose).toBeUndefined();
});

test("a transcript another session already mirrors is refused", async () => {
	resolveCalls = 0; nextPath = "/t/taken.jsonl";
	const owner = { sessionId: "owner", chatTranscriptPath: "/t/taken.jsonl" } as unknown as SessionRecord;
	const h = harness(4, [owner]);
	_ensureChatWatch.call(h.mgr, 4);
	await flush();
	expect(h.rec.chatWatchDispose).toBeUndefined();
	expect(h.rec.chatTranscriptPath).toBeUndefined();
	// No reset frame: the second pane keeps showing its terminal instead of
	// mirroring a conversation that belongs to another session.
	expect(h.frames).toHaveLength(0);
});

test("run state rides the title separator and publishes only on change", async () => {
	resolveCalls = 0; nextPath = "/t/runstate.jsonl";
	const h = harness(5);
	_ensureChatWatch.call(h.mgr, 5);
	await flush();
	const append = tailers.get("/t/runstate.jsonl")!;
	append([{ id: "m1", parentId: "", role: "user", ts: "2026-07-25T00:00:00Z", blocks: [{ kind: "text", text: "hi" }] }], 1);
	// The title is idle, so every frame so far reports an idle turn.
	expect(h.frames.every((f) => f.streaming === false)).toBe(true);

	const before = h.frames.length;
	h.mgr.lastOscTitle.set(5, WORKING);
	_emitChatRunState.call(h.mgr, 5);
	expect(h.frames).toHaveLength(before + 1);
	const flip = h.frames.at(-1)!;
	expect(flip.streaming).toBe(true);
	expect(flip.append).toHaveLength(0);   // payload-less: nothing was appended
	expect(flip.reset).toBe(false);        // and nothing is wiped
	expect(flip.seq).toBe(1);              // holds the transcript's seq

	// omp re-emits its title per spinner tick; every Braille frame is `working`.
	for (let i = 0; i < 12; i++) _emitChatRunState.call(h.mgr, 5);
	expect(h.frames).toHaveLength(before + 1);

	// `!` is a distinct state (agent blocked on the user) but not a running turn.
	h.mgr.lastOscTitle.set(5, ATTENTION);
	_emitChatRunState.call(h.mgr, 5);
	expect(h.frames).toHaveLength(before + 2);
	expect(h.frames.at(-1)!.streaming).toBe(false);
	_disposeChatWatch(h.rec);
});

test("dispose releases the transcript so a later session can bind it", async () => {
	resolveCalls = 0; nextPath = "/t/released.jsonl";
	const first = harness(6);
	_ensureChatWatch.call(first.mgr, 6);
	await flush();
	expect(first.rec.chatTranscriptPath).toBe("/t/released.jsonl");
	_disposeChatWatch(first.rec);
	expect(first.rec.chatTranscriptPath).toBeNull();

	// The refusal above keys off chatTranscriptPath, so a stale one would lock
	// the file out for every future session.
	const second = harness(7, [first.rec]);
	_ensureChatWatch.call(second.mgr, 7);
	await flush();
	expect(second.rec.chatWatchDispose).toBeDefined();
	_disposeChatWatch(second.rec);
});
