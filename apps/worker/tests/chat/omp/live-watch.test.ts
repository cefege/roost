// Live-bridge sidecar contract — the streaming half of the omp chat pane,
// driven by a hand-written NDJSON file in a temp dir (no omp, no extension, no
// PTY: the sidecar format IS the interface between them).
//
// What this pins, and why each would otherwise silently regress:
//   - ONE row per turn: message_start + N message_updates + message_end all
//     carry the same provisional id, so the pane repaints in place instead of
//     stacking a row per token — and the last one holds the whole text;
//   - `streaming` comes from the bridge, not from guessing at omp's OSC title:
//     agent_start → true, agent_end → false;
//   - the live→transcript JOIN: message_end names the omp entry id the message
//     was persisted under, and the tailer rewrites the transcript copy's id back
//     to the streamed one — ONE row, not two, when the durable copy lands;
//   - a bridge that dies mid-turn takes its half-streamed row with it, or that
//     frozen partial sits beside the transcript's later copy of the same turn;
//   - a second omp in the pane truncates the sidecar: the tailer must reseed AND
//     keep minting fresh ids, or the new conversation's first row overwrites the
//     previous one (both sides are upsert-by-id).

import { test, expect, afterAll } from "bun:test";
import { appendFileSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ChatMessage } from "@roost/shared/chat/wire";
import type { SessionRecord } from "../../../src/session-record.ts";
import { startLiveWatcher, type LiveEvent } from "../../../src/chat/omp/live-watcher.ts";
import {
	claimJoinKey, dropChatMessage, resolveLiveId, upsertChatMessage,
} from "../../../src/chat/omp/chat-record.ts";
import { ompLineJoinKey, parseOmpLine } from "../../../src/chat/omp/parse.ts";

const tmp = mkdtempSync(join(tmpdir(), "roost-live-watch-"));
afterAll(() => { rmSync(tmp, { recursive: true, force: true }); });

function line(obj: unknown): string {
	return `${JSON.stringify(obj)}\n`;
}

/** An omp assistant message as the bridge forwards it (ExtensionAPI shape).
 *  `timestamp` is epoch ms on the MESSAGE — deliberately unlike the ISO
 *  timestamp on the transcript ENTRY that wraps it, because conflating the two
 *  is exactly how the join key goes silently wrong. */
function assistant(text: string, stopReason = "stop"): Record<string, unknown> {
	return {
		role: "assistant", content: [{ type: "text", text }], timestamp: 1784836385900,
		provider: "anthropic", model: "claude-opus-5", responseId: "msg_011CdNn5", stopReason,
	};
}

interface Sink {
	events: LiveEvent[];
	onEmit: (ev: LiveEvent) => void;
	/** Settle when `cond` holds — driven by the watcher's own callback, so
	 *  nothing here waits on a guessed duration. A condition that never holds
	 *  hangs until bun's per-test timeout, which names the test. */
	until: (cond: () => boolean) => Promise<void>;
}

function sink(): Sink {
	const events: LiveEvent[] = [];
	let pending: { cond: () => boolean; resolve: () => void } | null = null;
	return {
		events,
		onEmit: (ev) => {
			events.push(ev);
			if (pending?.cond() === true) { pending.resolve(); pending = null; }
		},
		until: (cond) => {
			if (cond()) return Promise.resolve();
			const { promise, resolve } = Promise.withResolvers<void>();
			pending = { cond, resolve };
			return promise;
		},
	};
}

test("a streamed turn is ONE growing row, and the transcript's copy replaces it", async () => {
	const path = join(tmp, "turn.ndjson");
	// hello first: the watcher must attach to a file that already exists, the
	// same as a pane whose omp booted before the session record did.
	writeFileSync(path, line({
		t: "hello", v: 1, sid: "s1", sessionFile: "/omp/s1.jsonl", cwd: "/w", pid: 4242,
		ts: "2026-07-25T00:00:00.000Z",
	}));

	const s = sink();
	const w = startLiveWatcher(path, s.onEmit);
	try {
		await s.until(() => s.events.some((e) => e.kind === "hello"));
		expect(s.events[0]).toEqual({ kind: "hello", sessionFile: "/omp/s1.jsonl", pid: 4242 });

		appendFileSync(path, [
			line({ t: "ev", seq: 1, e: { type: "agent_start" } }),
			line({ t: "ev", seq: 2, live: "live-1", e: { type: "message_start", message: assistant("") } }),
			line({ t: "ev", seq: 3, live: "live-1", e: { type: "message_update", message: assistant("Par") } }),
			line({ t: "ev", seq: 4, live: "live-1", e: { type: "message_update", message: assistant("Parity ") } }),
			line({ t: "ev", seq: 5, live: "live-1", e: { type: "message_update", message: assistant("Parity hold") } }),
			line({ t: "ev", seq: 6, live: "live-1", entryId: "e1", e: { type: "message_end", message: assistant("Parity holds.") } }),
			line({ t: "ev", seq: 7, e: { type: "agent_end" } }),
		].join(""));

		await s.until(() => s.events.some((e) => e.kind === "streaming" && !e.value));

		// (a) every streamed row is the SAME message, and the last one is whole.
		const msgs = s.events.filter((e) => e.kind === "message").map((e) => e.msg);
		expect(msgs.length).toBeGreaterThanOrEqual(3);
		expect([...new Set(msgs.map((m) => m.id))]).toEqual(["live-1"]);
		const last = msgs.at(-1)!;
		expect(last.role).toBe("assistant");
		expect(last.blocks).toEqual([{ kind: "text", text: "Parity holds." }]);

		// (b) run state is the bridge's own, in order.
		expect(s.events.filter((e) => e.kind === "streaming").map((e) => e.value)).toEqual([true, false]);

		// (c) THE JOIN. omp's `message_end` fires before the entry is appended, so
		// the sidecar's `entryId` names the PREVIOUS leaf and is ignored; both
		// sides derive omp's own persistence key from the message instead. This
		// asserts the two derivations AGREE — the live event and the transcript
		// entry that later carries the same message must produce one key.
		const join = s.events.find((e) => e.kind === "join")!;
		expect(join.liveId).toBe("live-1");
		expect(join.key).toBe("assistant:1784836385900:anthropic:claude-opus-5:msg_011CdNn5:stop");

		const rec = { chat_seq: 0 } as unknown as SessionRecord;
		for (const m of msgs) upsertChatMessage(rec, m, rec.chat_seq);
		expect(rec.chatMessages).toHaveLength(1);
		expect(claimJoinKey(rec, join.key, join.liveId)).toBeNull();   // live got here first

		// The transcript line for that same turn: a real entry envelope, whose
		// own id (`e1`) and ISO timestamp differ from anything the bridge sent.
		const durableLine = JSON.stringify({
			type: "message", id: "e1", parentId: "e0", timestamp: "2026-07-25T00:00:01.000Z",
			message: assistant("Parity holds."),
		});
		expect(ompLineJoinKey(durableLine)).toBe(join.key);   // the two sides agree

		const durable = parseOmpLine(durableLine)!;
		expect(durable.id).toBe("e1");
		durable.id = resolveLiveId(rec, ompLineJoinKey(durableLine) ?? "", durable.id);
		expect(durable.id).toBe("live-1");
		upsertChatMessage(rec, durable, 7);
		expect(rec.chatMessages).toHaveLength(1);                            // ONE row, not two
		expect(rec.chatMessages![0]!.ts).toBe("2026-07-25T00:00:01.000Z");   // canonical copy won
		expect(rec.chatMsgSeqs).toEqual([0]);                                // original slot kept
	} finally {
		w.dispose();
	}
}, 15_000);

// The MIRROR of the test above, and a real defect found by driving a live pane:
// the sidecar and the transcript are tailed by two independent poll loops that
// both reseed from offset 0 on attach, so the TRANSCRIPT can legitimately see a
// turn first (attaching to an omp that was already running). Before the join was
// made commutative this left two permanent rows for one turn — the oracle
// reported `extra: [{tool…},{assistant}]` against a real session.
test("the join is commutative: the transcript may see a turn before the bridge", () => {
	const rec = { chat_seq: 0 } as unknown as SessionRecord;
	const msg = assistant("Parity holds.");
	const durableLine = JSON.stringify({
		type: "message", id: "e9", parentId: "e8", timestamp: "2026-07-25T00:00:01.000Z", message: msg,
	});
	const key = ompLineJoinKey(durableLine)!;
	expect(key).toBeTruthy();

	// Transcript first: nothing has claimed this turn, so it keeps its own id
	// and claims the key.
	const durable = parseOmpLine(durableLine)!;
	durable.id = resolveLiveId(rec, key, durable.id);
	expect(durable.id).toBe("e9");
	expect(claimJoinKey(rec, key, durable.id)).toBeNull();
	upsertChatMessage(rec, durable, 1);

	// …then the bridge replays the SAME turn under a provisional id. The claim
	// fails, naming the row already on screen, so the streamed copy is dropped.
	const streamed: ChatMessage = {
		id: "live-1", parentId: "", ts: "2026-07-25T00:00:00.500Z", role: "assistant",
		synthetic: false, blocks: [{ kind: "text", text: "Parity holds." }],
	};
	upsertChatMessage(rec, streamed, 0);
	expect(rec.chatMessages).toHaveLength(2);            // both present for an instant
	const held = claimJoinKey(rec, key, streamed.id);
	expect(held).toBe("e9");                             // the transcript got there first
	expect(dropChatMessage(rec, streamed.id)).toBe(true);
	expect(rec.chatMessages).toHaveLength(1);            // ONE row, either order
	expect(rec.chatMessages![0]!.id).toBe("e9");         // canonical copy survives
});

// Regression, found by driving a REAL omp 17.1.3 through the bridge: pressing
// Esc makes omp emit `message_start` with errorMessage "Request was aborted"
// (errorId 0x08001800 = Abort|Class) — which renders as a red "Operation
// aborted" notice — and then `message_end` with `__omp.silent_abort__`, which
// the TUI paints as NOTHING. The row must be RETRACTED. Before the fix the pane
// kept the intermediate error row forever: a red line where the terminal is
// silent, i.e. exactly the parity break this whole feature exists to prevent.
test("a turn that ends in a silent abort retracts the row it already painted", async () => {
	const path = join(tmp, "silent-abort.ndjson");
	writeFileSync(path, "");
	const s = sink();
	const w = startLiveWatcher(path, s.onEmit);
	try {
		const aborting = {
			role: "assistant", content: [], timestamp: 1784836385901,
			provider: "anthropic", model: "claude-opus-5", responseId: "",
			stopReason: "aborted", errorMessage: "Request was aborted", errorId: 134221824,
		};
		const silent = { ...aborting, errorMessage: "__omp.silent_abort__" };

		appendFileSync(path, [
			line({ t: "hello", v: 1, sid: "s3", sessionFile: "/omp/s3.jsonl", cwd: "/w", pid: 9, ts: "x" }),
			line({ t: "ev", seq: 1, e: { type: "agent_start" } }),
			line({ t: "ev", seq: 2, live: "live-1", e: { type: "message_start", message: aborting } }),
			line({ t: "ev", seq: 3, live: "live-1", e: { type: "message_end", message: silent } }),
			line({ t: "ev", seq: 4, e: { type: "agent_end" } }),
		].join(""));

		await s.until(() => s.events.some((e) => e.kind === "retract"));

		// message_start DID paint a row — the notice omp's own
		// resolveAssistantErrorPresentation yields for a generic abort.
		const painted = s.events.filter((e) => e.kind === "message").map((e) => e.msg);
		expect(painted).toHaveLength(1);
		expect(painted[0]!.blocks).toEqual([{ kind: "notice", level: "error", text: "Operation aborted" }]);

		// …and message_end retracts exactly that row.
		const retract = s.events.find((e) => e.kind === "retract")!;
		expect(retract).toEqual({ kind: "retract", liveId: painted[0]!.id });

		const rec = { chat_seq: 0 } as unknown as SessionRecord;
		for (const m of painted) upsertChatMessage(rec, m, 0);
		expect(dropChatMessage(rec, retract.liveId)).toBe(true);
		expect(rec.chatMessages).toHaveLength(0);   // silent in the terminal, silent here

		// No join is recorded for a retracted row: there is nothing on screen for
		// the transcript's copy to replace.
		expect(s.events.some((e) => e.kind === "join")).toBe(false);
	} finally {
		w.dispose();
	}
}, 15_000);

test("a bridge that says goodbye mid-turn takes its half-streamed row with it", async () => {
	const path = join(tmp, "bye.ndjson");
	writeFileSync(path, "");
	const s = sink();
	const w = startLiveWatcher(path, s.onEmit);
	try {
		appendFileSync(path, [
			line({ t: "hello", v: 1, sid: "s2", sessionFile: "/omp/s2.jsonl", cwd: "/w", pid: 7, ts: "x" }),
			line({ t: "ev", seq: 1, e: { type: "agent_start" } }),
			line({ t: "ev", seq: 2, live: "live-1", e: { type: "message_start", message: assistant("half a th") } }),
			line({ t: "bye" }),
		].join(""));

		await s.until(() => s.events.some((e) => e.kind === "abort"));
		const abort = s.events.find((e) => e.kind === "abort")!;
		expect(abort).toEqual({ kind: "abort", liveId: "live-1" });

		// The row the pane already showed is dropped, so the transcript's later
		// copy of that turn is the only one left in the thread.
		const rec = { chat_seq: 0 } as unknown as SessionRecord;
		for (const e of s.events) if (e.kind === "message") upsertChatMessage(rec, e.msg, 0);
		expect(rec.chatMessages).toHaveLength(1);
		expect(dropChatMessage(rec, abort.liveId!)).toBe(true);
		expect(rec.chatMessages).toHaveLength(0);
		expect(rec.chatMsgSeqs).toHaveLength(0);
	} finally {
		w.dispose();
	}
}, 15_000);

test("narration the bridge forwards becomes notice rows, not prose the pane cannot style", async () => {
	const path = join(tmp, "narrate.ndjson");
	writeFileSync(path, "");
	const s = sink();
	const w = startLiveWatcher(path, s.onEmit);
	try {
		appendFileSync(path, [
			line({ t: "hello", v: 1, sid: "s4", sessionFile: "/omp/s4.jsonl", cwd: "/w", pid: 9, ts: "x" }),
			line({ t: "ev", seq: 1, e: { type: "auto_compaction_start" } }),
			line({ t: "ev", seq: 2, e: { type: "notice", level: "error", message: "upstream 500" } }),
			// Sidecar-only arms — omp's RPC mode emits neither. A retry that ended
			// and a reminder with nothing to say are silent in the TUI too.
			line({ t: "ev", seq: 3, e: { type: "auto_retry_end" } }),
			line({ t: "ev", seq: 4, e: { type: "todo_reminder" } }),
			line({ t: "ev", seq: 5, e: { type: "ttsr_triggered", reason: "context is stale" } }),
		].join(""));

		await s.until(() => s.events.filter((e) => e.kind === "message").length === 3);
		const rows = s.events.filter((e) => e.kind === "message").map((e) => e.msg);
		expect(rows.map((m) => m.role)).toEqual(["developer", "developer", "developer"]);
		expect(rows.map((m) => m.blocks)).toEqual([
			[{ kind: "notice", text: "— compacting context… —", level: "note" }],
			[{ kind: "notice", text: "error: upstream 500", level: "error" }],
			[{ kind: "notice", text: "context is stale", level: "note" }],
		]);
	} finally {
		w.dispose();
	}
}, 15_000);

test("a second omp in the same pane restarts the stream without clobbering the first", async () => {
	const path = join(tmp, "restart.ndjson");
	// Deliberately long: the bridge reopens the sidecar with "w", so the restart
	// below SHRINKS the file, which is the only signal the tailer has that the
	// bytes at its offset belong to a different conversation.
	writeFileSync(path, [
		line({ t: "hello", v: 1, sid: "s3", sessionFile: "/omp/a.jsonl", cwd: "/w", pid: 1, ts: "x" }),
		line({ t: "ev", seq: 1, live: "live-1", entryId: "e1", e: { type: "message_end", message: assistant("first ".repeat(80)) } }),
	].join(""));

	const s = sink();
	const w = startLiveWatcher(path, s.onEmit);
	try {
		await s.until(() => s.events.some((e) => e.kind === "join"));
		writeFileSync(path, [
			line({ t: "hello", v: 1, sid: "s3", sessionFile: "/omp/b.jsonl", cwd: "/w", pid: 2, ts: "y" }),
			line({ t: "ev", seq: 1, live: "live-1", entryId: "e9", e: { type: "message_end", message: assistant("second") } }),
		].join(""));

		await s.until(() => s.events.filter((e) => e.kind === "join").length === 2);
		expect(s.events.filter((e) => e.kind === "hello")).toHaveLength(2);

		// Distinct ids across the restart: re-minting `live-1` would make the new
		// omp's first row REPLACE the previous conversation's row in place.
		const msgs = s.events.filter((e) => e.kind === "message").map((e) => e.msg);
		expect(new Set(msgs.map((m) => m.id)).size).toBe(msgs.length);
		const rec = { chat_seq: 0 } as unknown as SessionRecord;
		for (const m of msgs) upsertChatMessage(rec, m, 0);
		expect(rec.chatMessages).toHaveLength(2);
	} finally {
		w.dispose();
	}
}, 15_000);
