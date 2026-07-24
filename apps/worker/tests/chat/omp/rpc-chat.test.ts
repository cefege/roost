// Native RPC chat protocol proof — the worker's omp event mapping driven by a
// real child process speaking the real JSONL grammar (fixtures/fake-omp.ts,
// selected via ROOST_OMP_BIN). No omp install, no credentials, no PTY.
//
// What this pins, and why each would otherwise silently regress:
//   - streaming upsert: N message_updates + message_end collapse to ONE chat
//     message holding the complete text (append-only would leave 4 rows);
//   - tool lifecycle: start+end collapse to one message that ends at phase:"end";
//   - approvals: the block arrives unresolved and the SAME id re-emits resolved
//     after the pane answers (extension_ui_response bypasses id correlation);
//   - mid-turn prompt: the worker injects streamingBehavior:"followUp" — omp
//     REJECTS a bare prompt while streaming;
//   - respawn: a dead child's replacement issues switch_session BEFORE the
//     prompt, or the message runs in a fresh, empty conversation.

import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, readFileSync, existsSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ChatFrame, ChatMessage, ContentBlock } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../../src/session-record.ts";
import { rpcChatCommand, disposeRpcChat, disposeAllRpcChats, type RpcChatHost } from "../../../src/chat/omp/rpc-chat.ts";
import { loadOmpSessionFile, forgetOmpSession, _resetOmpSessionStoreCache } from "../../../src/chat/omp/session-store.ts";

const FAKE = join(import.meta.dir, "fixtures", "fake-omp.ts");
process.env.ROOST_OMP_BIN = FAKE;

const tmp = mkdtempSync(join(tmpdir(), "roost-rpc-chat-"));
// Redirect the durable sessionId→sessionFile store off the real worker data
// dir — without this the suite would clobber the running worker's state.
process.env.ROOST_WORKER_DATA_DIR = tmp;
afterAll(() => { rmSync(tmp, { recursive: true, force: true }); });

interface Harness {
	rec: SessionRecord;
	host: RpcChatHost;
	frames: ChatFrame[];
	log(): Record<string, unknown>[];
}

function harness(sid: string): Harness {
	const logPath = join(tmp, `${sid}.log`);
	process.env.FAKE_OMP_LOG = logPath;
	process.env.FAKE_OMP_SESSION_FILE = join(tmp, `${sid}-session.jsonl`);
	const frames: ChatFrame[] = [];
	const rec = {
		sessionId: sid, channelId: 1, cwd: tmp, chat_seq: 0,
		chatMessages: [] as ChatMessage[], chatMsgSeqs: [] as number[],
	} as unknown as SessionRecord;
	return {
		rec, frames,
		host: {
			getBySessionId: (id) => (id === sid ? rec : undefined),
			sendChatFrameUpstream: (_c, f) => { frames.push(f); },
		},
		log: () => (existsSync(logPath) ? readFileSync(logPath, "utf8").split("\n").filter(Boolean).map((l) => JSON.parse(l) as Record<string, unknown>) : []),
	};
}

/** Poll for a condition driven by a REAL child process over stdio. Fake timers
 *  cannot advance another process's clock, so this integration test polls the
 *  observable state instead of sleeping a guessed duration. */
async function waitFor(what: string, cond: () => boolean, ms = 5000): Promise<void> {
	const deadline = Date.now() + ms;
	while (Date.now() < deadline) {
		if (cond()) return;
		await Bun.sleep(10);
	}
	throw new Error(`timeout waiting for ${what}`);
}

/** First block of the message holding `kind`, narrowed. */
function findBlock<K extends ContentBlock["kind"]>(msgs: ChatMessage[], kind: K): Extract<ContentBlock, { kind: K }> | undefined {
	for (const m of msgs) for (const b of m.blocks) if (b.kind === kind) return b as Extract<ContentBlock, { kind: K }>;
	return undefined;
}

test("streaming turn: upsert, tool collapse, approval round trip, mid-turn prompt", async () => {
	const h = harness("s-stream");
	try {
		const res = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "hi" }));
		expect(res.ok).toBe(true);

		// get_state resolved the session file before the first command went out.
		expect(h.rec.chatTranscriptPath).toBe(join(tmp, "s-stream-session.jsonl"));

		await waitFor("approval block", () => findBlock(h.rec.chatMessages ?? [], "approval") !== undefined);
		const msgs = h.rec.chatMessages ?? [];

		// 3 message_updates + message_end → ONE row with the complete text.
		const textRows = msgs.filter((m) => m.role === "assistant" && m.blocks.some((b) => b.kind === "text"));
		expect(textRows.length).toBe(1);
		expect(textRows[0]!.blocks[0]).toEqual({ kind: "text", text: "Hello, world" });

		// start + 3 live updates + end → ONE row, final phase "end". The live
		// output is cleared by the terminal state (the real result follows as a
		// toolResult message), so its absence here is the contract, not a miss.
		const toolRows = msgs.filter((m) => m.blocks.some((b) => b.kind === "toolEvent"));
		expect(toolRows.length).toBe(1);
		expect(findBlock(msgs, "toolEvent")).toEqual({ kind: "toolEvent", callId: "call_1", name: "read", phase: "end", intent: "", output: "" });

		// The coalesced live frames must have carried the NEWEST partial, not the
		// first — a leading-edge drop would strand "line 1" on screen.
		const liveFrames = h.frames.filter((f) =>
			f.append.some((m) => m.blocks.some((b) => b.kind === "toolEvent" && b.phase === "update")));
		expect(liveFrames.length).toBeGreaterThan(0);
		const lastLive = liveFrames.at(-1)!.append[0]!.blocks[0];
		expect(lastLive.kind === "toolEvent" && lastLive.output).toBe("line 1\nline 2\nline 3\n");
		// Coalescing actually happened: 3 updates did not become 3 frames.
		expect(liveFrames.length).toBeLessThan(3);

		// Session status the omp TUI shows rides every frame.
		expect(h.frames.at(-1)!.model).toBe("anthropic/claude-opus-5");
		expect(h.frames.at(-1)!.contextTokens).toBe(18004);
		expect(h.frames.at(-1)!.contextPct).toBe(2);   // 1.8004 → rounded, as omp's own /context does

		// Approval arrives unresolved; the turn is still open.
		const approvalMsg = msgs.find((m) => m.blocks.some((b) => b.kind === "approval"))!;
		expect(findBlock(msgs, "approval")).toEqual({
			kind: "approval", requestId: "ui-1", method: "confirm",
			title: "Confirm", message: "Continue?", options: [], resolved: false, answer: "",
		});
		expect(h.frames.at(-1)!.streaming).toBe(true);

		// A prompt sent mid-turn must reach omp WITH streamingBehavior — a bare
		// prompt is rejected by the real agent while streaming.
		const mid = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "and risks?" }));
		expect(mid.ok).toBe(true);
		const midFrame = h.log().find((f) => f.type === "prompt" && f.message === "and risks?");
		expect(midFrame?.streamingBehavior).toBe("followUp");

		// Answering re-emits the SAME message id, now resolved.
		const ans = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "extension_ui_response", id: "ui-1", confirmed: true }));
		expect(ans.ok).toBe(true);
		const resolved = (h.rec.chatMessages ?? []).filter((m) => m.blocks.some((b) => b.kind === "approval"));
		expect(resolved.length).toBe(1);
		expect(resolved[0]!.id).toBe(approvalMsg.id);
		expect(findBlock(resolved, "approval")).toMatchObject({ resolved: true, answer: "approved" });

		// agent_end closes the turn and publishes streaming:false.
		await waitFor("turn end", () => h.frames.at(-1)?.streaming === false);

		// An unknown ui id is refused rather than posted blind.
		const bogus = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "extension_ui_response", id: "nope", confirmed: true }));
		expect(bogus).toEqual({ ok: false, error: "unknown ui request" });
	} finally {
		disposeRpcChat("s-stream");
	}
});

test("respawn resumes the prior omp session before the new prompt runs", async () => {
	const h = harness("s-respawn");
	try {
		await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "__die" }));
		await waitFor("child exit row", () =>
			(h.rec.chatMessages ?? []).some((m) => m.blocks.some((b) => b.kind === "text" && b.text.includes("agent process exited"))));

		const res = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "still there?" }));
		expect(res.ok).toBe(true);

		// The replacement child's log: switch_session, then history paging, and
		// only then the prompt. Ordering is the whole point — a prompt written
		// first would run in a fresh, empty session.
		const types = h.log().map((f) => String(f.type));
		const switchAt = types.indexOf("switch_session");
		const promptAt = types.lastIndexOf("prompt");
		expect(switchAt).toBeGreaterThanOrEqual(0);
		expect(promptAt).toBeGreaterThan(switchAt);
		expect(types.indexOf("get_messages_page")).toBeGreaterThan(switchAt);

		// Paged history was re-seeded into the transcript after a reset frame.
		expect((h.rec.chatMessages ?? []).some((m) =>
			m.role === "user" && m.blocks.some((b) => b.kind === "text" && b.text === "resumed history"))).toBe(true);
		expect(h.frames.some((f) => f.reset && f.seq >= 0)).toBe(true);
	} finally {
		disposeRpcChat("s-respawn");
	}
});

test("disallowed commands never reach the child", async () => {
	const h = harness("s-guard");
	try {
		expect(await rpcChatCommand(h.host, h.rec, "not json")).toEqual({ ok: false, error: "invalid command JSON" });
		expect(await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "bash", command: "rm -rf /" })))
			.toEqual({ ok: false, error: "command not allowed: bash" });
		expect(h.log().length).toBe(0);
	} finally {
		disposeRpcChat("s-guard");
	}
});

test("worker restart resumes: mapping outlives the process, not just the child", async () => {
	const h = harness("s-restart");
	try {
		await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "hi" }));
		const file = join(tmp, "s-restart-session.jsonl");
		expect(loadOmpSessionFile("s-restart")).toBe(file);

		// Simulate a worker RESTART: children killed, in-memory entries gone,
		// store cache cold — exactly what a fresh process sees. The mapping is
		// the only thing that survives, and it must be enough.
		disposeAllRpcChats();
		_resetOmpSessionStoreCache();
		expect(loadOmpSessionFile("s-restart")).toBe(file);

		const before = h.log().length;
		const res = await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "still there?" }));
		expect(res.ok).toBe(true);
		const after = h.log().map((f) => String(f.type)).slice(before);
		expect(after.indexOf("switch_session")).toBe(0);
		expect(after.lastIndexOf("prompt")).toBeGreaterThan(after.indexOf("switch_session"));
	} finally {
		disposeRpcChat("s-restart");
	}
});

test("respawn keeps the mapping; only a true close forgets it", async () => {
	const h = harness("s-keep");
	try {
		await rpcChatCommand(h.host, h.rec, JSON.stringify({ type: "prompt", message: "hi" }));
		expect(loadOmpSessionFile("s-keep")).not.toBeNull();

		// _dropChannelState also fires on respawn (session-resume.ts:173) for a
		// sessionId that comes straight back — a keeper hiccup must NOT wipe the
		// conversation, so disposeRpcChat kills the child and nothing else.
		disposeRpcChat("s-keep");
		expect(loadOmpSessionFile("s-keep")).not.toBeNull();

		// closedByKeeper — the pane is gone for good.
		forgetOmpSession("s-keep");
		expect(loadOmpSessionFile("s-keep")).toBeNull();
	} finally {
		disposeRpcChat("s-keep");
	}
});
