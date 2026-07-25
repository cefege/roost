// omp transcript discovery — the resume picker's data source.
//
// What this pins, and why each would otherwise silently regress:
//   - the `{"type":"session"}` header is REQUIRED: it is the only positive proof
//     a *.jsonl is omp's, and without the check the picker lists every JSONL on
//     the machine (npm logs, jsonl datasets, other tools' transcripts);
//   - only the head and tail of a file are read: transcripts run to hundreds of
//     megabytes and a full read of the newest 50 stalls the worker. The test
//     builds a file bigger than the head window and still expects both a title
//     (from the head) and the newest prompt (from the tail);
//   - a timestamp/hex title is rejected in favour of the first user message —
//     "2026-07-25T10-00-00" is a filename, not a name;
//   - the ACTIVE window: a transcript a live omp is still writing must be
//     REFUSED, because two writers corrupt a session file. This is the one real
//     hazard in the whole resume path.

import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, utimesSync, statSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
	listOmpSessions, describeSession, resolveSessionDir, resumeBlockedReason, ACTIVE_WINDOW_MS,
} from "../../../src/chat/omp/session-discovery.ts";

const tmp = mkdtempSync(join(tmpdir(), "roost-omp-discovery-"));
afterAll(() => { rmSync(tmp, { recursive: true, force: true }); });

function write(rel: string, lines: unknown[], ageMs = 10 * 60_000): string {
	const path = join(tmp, rel);
	mkdirSync(join(path, ".."), { recursive: true });
	writeFileSync(path, `${lines.map((l) => JSON.stringify(l)).join("\n")}\n`);
	const t = new Date(Date.now() - ageMs);
	utimesSync(path, t, t);
	return path;
}

test("a file without an omp session header is not an omp transcript", () => {
	const path = write("stray/not-omp.jsonl", [{ level: "info", msg: "some other tool" }]);
	expect(describeSession(path, statSync(path).mtimeMs)).toBeNull();
});

test("title comes from session_info, else the first user message", () => {
	const named = write("a/named.jsonl", [
		{ type: "session", id: "a", cwd: "/work/proj" },
		{ type: "session_info", name: "Divider drag fix" },
		{ type: "message", role: "user", content: "make the divider draggable" },
	]);
	const e = describeSession(named, statSync(named).mtimeMs)!;
	expect(e.title).toBe("Divider drag fix");
	expect(e.cwd).toBe("/work/proj");

	const unnamed = write("a/unnamed.jsonl", [
		{ type: "session", id: "b", cwd: "/work/other" },
		{ type: "message", role: "user", content: "  why is   the grid   blank?\nmore detail " },
	]);
	// Whitespace-collapsed, so a multi-line prompt is one readable line.
	expect(describeSession(unnamed, statSync(unnamed).mtimeMs)!.title).toBe("why is the grid blank? more detail");
});

test("a timestamp or hex title is rejected — that is a filename, not a name", () => {
	const stamped = write("a/stamped.jsonl", [
		{ type: "session", id: "c", cwd: "/w" },
		{ type: "session_info", name: "2026-07-25T10-00-00" },
		{ type: "message", role: "user", content: "real question here" },
	]);
	expect(describeSession(stamped, statSync(stamped).mtimeMs)!.title).toBe("real question here");

	const hexed = write("a/hexed.jsonl", [
		{ type: "session", id: "d", cwd: "/w" },
		{ type: "title", title: "deadbeefcafe1234" },
		{ type: "message", role: "user", content: "also real" },
	]);
	expect(describeSession(hexed, statSync(hexed).mtimeMs)!.title).toBe("also real");
});

test("a huge transcript is summarised without reading it whole", () => {
	// > the 64 KiB head window, so the tail read is a genuinely separate slice.
	const filler = Array.from({ length: 400 }, (_, i) => ({
		type: "message", role: "assistant", content: `${"x".repeat(400)} ${i}`,
	}));
	const path = write("big/huge.jsonl", [
		{ type: "session", id: "e", cwd: "/work/big" },
		{ type: "message", role: "user", content: "the opening question" },
		...filler,
		{ type: "message", role: "user", content: "the newest question" },
	]);
	expect(statSync(path).size).toBeGreaterThan(64 * 1024);
	const e = describeSession(path, statSync(path).mtimeMs)!;
	// Title from the HEAD, newest prompt from the TAIL — the two slices.
	expect(e.title).toBe("the opening question");
	expect(e.lastPrompt).toBe("the newest question");
});

test("listing walks nested dirs, ranks by mtime, honours the limit, and skips non-omp files", () => {
	const dir = mkdtempSync(join(tmp, "sessions-"));
	const mk = (name: string, ageMs: number): string => {
		const p = join(dir, name);
		mkdirSync(join(p, ".."), { recursive: true });
		writeFileSync(p, `${JSON.stringify({ type: "session", id: name, cwd: "/w" })}\n${JSON.stringify({ type: "message", role: "user", content: name })}\n`);
		const t = new Date(Date.now() - ageMs);
		utimesSync(p, t, t);
		return p;
	};
	const newest = mk("-Code-idea/newest.jsonl", 60_000);
	mk("-Code-idea/older.jsonl", 3_600_000);
	mk("deep/deeper/oldest.jsonl", 86_400_000);
	writeFileSync(join(dir, "notes.txt"), "not jsonl");

	process.env.OMP_SESSION_DIR = dir;
	try {
		expect(resolveSessionDir()).toBe(dir);
		const all = listOmpSessions();
		expect(all.map((s) => s.title)).toEqual(["-Code-idea/newest.jsonl", "-Code-idea/older.jsonl", "deep/deeper/oldest.jsonl"]);
		expect(all[0]!.path).toBe(newest);
		expect(listOmpSessions(2)).toHaveLength(2);
	} finally {
		delete process.env.OMP_SESSION_DIR;
	}
});

test("a missing session dir is an empty list, not a throw", () => {
	process.env.OMP_SESSION_DIR = join(tmp, "does-not-exist");
	try { expect(listOmpSessions()).toEqual([]); }
	finally { delete process.env.OMP_SESSION_DIR; }
});

test("a transcript a live omp is still writing is marked active and refused", () => {
	const hot = write("hot/live.jsonl", [
		{ type: "session", id: "f", cwd: "/w" },
		{ type: "message", role: "user", content: "still going" },
	], 1_000);
	expect(describeSession(hot, statSync(hot).mtimeMs)!.active).toBe(true);
	expect(resumeBlockedReason(hot)).toMatch(/looks active/);

	// Just past the window it is fair game — that is the whole decision.
	const cold = write("hot/cold.jsonl", [
		{ type: "session", id: "g", cwd: "/w" },
		{ type: "message", role: "user", content: "done for now" },
	], ACTIVE_WINDOW_MS + 5_000);
	expect(describeSession(cold, statSync(cold).mtimeMs)!.active).toBe(false);
	expect(resumeBlockedReason(cold)).toBeNull();

	// A path that does not exist is NOT a refusal: a stale bookmark degrades to
	// a fresh conversation (rpc-chat.ts says so with a notice row).
	expect(resumeBlockedReason(join(tmp, "gone.jsonl"))).toBeNull();
});

test("a project-local .omp/settings.json sessionDir beats the global one", () => {
	const cwd = mkdtempSync(join(tmp, "proj-"));
	mkdirSync(join(cwd, ".omp"), { recursive: true });
	writeFileSync(join(cwd, ".omp", "settings.json"), JSON.stringify({ sessionDir: "/local/sessions" }));
	expect(resolveSessionDir(cwd)).toBe("/local/sessions");
	// OMP_SESSION_DIR still outranks it — an explicit env var is the operator
	// speaking, and it wins over any file.
	process.env.OMP_SESSION_DIR = "/env/sessions";
	try { expect(resolveSessionDir(cwd)).toBe("/env/sessions"); }
	finally { delete process.env.OMP_SESSION_DIR; }
});
