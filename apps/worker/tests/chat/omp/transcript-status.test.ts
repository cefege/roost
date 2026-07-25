// Status folding in the REAL tailer, against a real file on disk.
//
// mirror-watch.test.ts mocks startTranscriptWatcher wholesale, so nothing there
// touches the loop that actually produces the chat pane's status row. The two
// behaviours pinned here are what make the feature work, and each has a
// plausible one-line regression no other test would catch:
//
//   - a bare metadata line (mode_change) emits a frame with an EMPTY batch.
//     Reverting the emit condition to `batch.length > 0` strands the Plan chip
//     until the next assistant turn — minutes, or forever. The snapshot also
//     ACCUMULATES: one line updates one fact, the rest persist.
//   - truncation/rotation RESETS the snapshot, so a fact the replay never
//     restates cannot linger from the previous file.
//
// Real fs + real fs.watch on purpose: the code under test is an OS-notification
// tailer, and the thing most likely to break is its interaction with actual
// file growth and truncation. Fake timers cannot drive a kernel watch event, so
// `until` awaits the real callback with a deadline rather than sleeping a fixed
// "long enough" — no assertion here depends on how long anything took.

import { test, expect } from "bun:test";
import { mkdtemp, writeFile, appendFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startTranscriptWatcher, emptyOmpStatus, foldOmpStatus, type OmpStatus } from "../../../src/chat/omp/transcript-watcher.ts";

// Verbatim shapes from the live corpus (see parse.test.ts).
const MODEL = `{"type":"model_change","id":"m1","parentId":null,"timestamp":"2026-07-25T09:30:08.207Z","model":"anthropic/claude-opus-5"}`;
const MODE_PLAN = `{"type":"mode_change","id":"m2","parentId":"m1","timestamp":"2026-07-25T09:30:08.326Z","mode":"plan","data":{"planFilePath":"local://PLAN.md"}}`;
const THINKING = `{"type":"thinking_level_change","id":"m3","parentId":"m2","timestamp":"2026-07-25T09:31:11.851Z","thinkingLevel":"low","configured":"auto"}`;

/** One captured callback. `status` is COPIED: the tailer mutates a single
 *  snapshot in place, so holding the reference would make every captured entry
 *  read as the final state. */
interface Capture { count: number; seq: number; status: OmpStatus }

/** Resolve once `n` callbacks have landed, else fail naming what was awaited. */
async function until(got: Capture[], n: number, what: string): Promise<void> {
	const deadline = Date.now() + 5000;
	while (got.length < n && Date.now() < deadline) await Bun.sleep(10);
	if (got.length < n) throw new Error(`timed out waiting for ${what} (got ${got.length}/${n})`);
}

async function harness(initial: string): Promise<{ path: string; got: Capture[]; stop: () => Promise<void> }> {
	const dir = await mkdtemp(join(tmpdir(), "roost-status-"));
	const path = join(dir, "session.jsonl");
	await writeFile(path, initial);
	const got: Capture[] = [];
	const handle = startTranscriptWatcher(path, (msgs, seq, status) => {
		got.push({ count: msgs.length, seq, status: { ...status } });
	});
	return {
		path, got,
		stop: async () => { handle.dispose(); await rm(dir, { recursive: true, force: true }); },
	};
}

test("a bare mode_change emits a frame carrying NO messages", async () => {
	const h = await harness(MODEL + "\n");
	try {
		await until(h.got, 1, "the initial read");
		expect(h.got[0]!.status.model).toBe("anthropic/claude-opus-5");
		expect(h.got[0]!.count).toBe(0);   // model_change is not a chat message

		await appendFile(h.path, MODE_PLAN + "\n");
		await until(h.got, 2, "the mode_change frame");
		const last = h.got.at(-1)!;
		expect(last.count).toBe(0);
		expect(last.status.mode).toBe("plan");
		// Accumulated, not replaced: the model survives a mode-only line.
		expect(last.status.model).toBe("anthropic/claude-opus-5");
	} finally { await h.stop(); }
});

test("truncation resets the snapshot instead of carrying stale facts over", async () => {
	const h = await harness(MODEL + "\n" + MODE_PLAN + "\n" + THINKING + "\n");
	try {
		await until(h.got, 1, "the initial read");
		expect(h.got.at(-1)!.status.mode).toBe("plan");
		expect(h.got.at(-1)!.status.thinkingLevel).toBe("low");

		// Rotate: a SHORTER file whose replay never mentions plan mode. Without
		// the reset the Plan chip would outlive the session that set it.
		await writeFile(h.path, MODEL + "\n");
		await until(h.got, h.got.length + 1, "the post-truncation frame");
		const last = h.got.at(-1)!;
		expect(last.status.mode).toBe("");
		expect(last.status.thinkingLevel).toBe("");
		expect(last.status.model).toBe("anthropic/claude-opus-5");
		expect(last.seq).toBe(1);          // line count reseeded with the file
	} finally { await h.stop(); }
});

// ── the change-gate, unit-tested ─────────────────────────────────────────────
// Pinned on the pure fold rather than through the tailer: proving "no frame was
// emitted" via the fs path needs a negative wall-clock wait, which is exactly
// the kind of tuned sleep that flakes under load.

test("folding a delta reports whether anything actually changed", () => {
	const s = emptyOmpStatus();
	expect(foldOmpStatus(s, { mode: "plan" })).toBe(true);
	expect(s.mode).toBe("plan");
	// omp re-announces the mode on re-entry, and every transcript re-read
	// replays lines already folded — neither may force an empty frame.
	expect(foldOmpStatus(s, { mode: "plan" })).toBe(false);
	expect(foldOmpStatus(s, { mode: "none" })).toBe(true);
});

test("folding is per-field: an absent field leaves its fact standing", () => {
	const s = emptyOmpStatus();
	foldOmpStatus(s, { model: "anthropic/claude-opus-5", contextTokens: 1000 });
	expect(foldOmpStatus(s, { thinkingLevel: "low" })).toBe(true);
	expect(s).toEqual({
		model: "anthropic/claude-opus-5", mode: "", thinkingLevel: "low", contextTokens: 1000,
	});
	// A genuine 0 is a real value, not "unknown": clearing the count after a
	// compaction must register as a change.
	expect(foldOmpStatus(s, { contextTokens: 0 })).toBe(true);
	expect(s.contextTokens).toBe(0);
});
