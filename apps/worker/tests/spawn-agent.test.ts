// Web-UI mode spawn proof: `spawnAgent` makes a session whose PROCESS is the
// omp RPC child, with no PTY anywhere in it.
//
// What this pins, and why each would otherwise silently regress:
//   - the row is kind:"agent" and carries NO wtermCore / keeper channel — the
//     moment one appears, terminal mode and web-UI mode share runtime state
//     again, which is the entanglement this whole split removes;
//   - the child is live at spawn, not on the first prompt: a row whose process
//     only materialises later is a breadcrumb, not a session;
//   - the PTY-side sweeps (_runDetect, claimViewport) walk EVERY session on a
//     timer and used to deref wtermCore unconditionally — they must no-op here
//     rather than throw;
//   - a missing omp binary fails the spawn instead of leaving a half-live row;
//   - a resume path that cannot be read degrades to a fresh conversation plus
//     one notice, because a stale bookmark must never block making a session.
//
// Driven by the same fake omp the RPC protocol tests use (fixtures/fake-omp.ts
// via ROOST_OMP_BIN) — no omp install, no credentials, no keeper.

import { test, expect, afterAll } from "bun:test";
import { mkdtempSync, rmSync, writeFileSync, utimesSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { asWorkerFp, type SessionEvent } from "@roost/shared";
import { SessionManager } from "../src/session-manager.ts";
import { rpcChatActive, disposeRpcChat } from "../src/chat/omp/rpc-chat.ts";

const FAKE = join(import.meta.dir, "chat", "omp", "fixtures", "fake-omp.ts");
process.env.ROOST_OMP_BIN = FAKE;

const tmp = mkdtempSync(join(tmpdir(), "roost-spawn-agent-"));
process.env.ROOST_WORKER_DATA_DIR = tmp;
process.env.FAKE_OMP_LOG = join(tmp, "child.log");
process.env.FAKE_OMP_SESSION_FILE = join(tmp, "child-session.jsonl");
afterAll(() => { rmSync(tmp, { recursive: true, force: true }); });

function freshMgr(events: SessionEvent[]): SessionManager {
	return new SessionManager({
		workerFp: asWorkerFp("00".repeat(32)),
		sink: { emit: (e) => events.push(e) },
		hookSocketPath: "/dev/null",
	});
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

test("an agent session owns an omp child and holds no PTY state", async () => {
	const events: SessionEvent[] = [];
	const mgr = freshMgr(events);
	try {
		const rec = await mgr.spawnAgent(tmp);

		expect(rec.kind).toBe("agent");
		// The two things that would re-entangle the subsystems.
		expect(rec.wtermCore).toBeUndefined();
		expect(rec.childPid).toBeUndefined();

		const opened = events.find((e) => e.kind === "opened");
		expect(opened).toMatchObject({ kind: "opened", session_kind: "agent", cwd: tmp });

		// The child is live NOW — spawn is the trigger, not the first prompt.
		expect(rpcChatActive(String(rec.sessionId))).toBe(true);

		// The timer-driven PTY sweeps walk every session; both must skip this one
		// rather than deref a core that does not exist.
		expect(() => { mgr._runDetect(rec.channelId); }).not.toThrow();
		expect(() => { mgr.claimViewport(rec.channelId, "viewer-1", 120, 40); }).not.toThrow();
		expect(mgr.viewportClaims.get(rec.channelId)).toBeUndefined();
		expect(() => mgr.diagSnapshot()).not.toThrow();

		// It answers RPC: get_state round-trips through the real stdio protocol.
		await waitFor("session file resolved", () => rec.chatTranscriptPath !== undefined && rec.chatTranscriptPath !== null);
		expect(rec.chatTranscriptPath).toBe(join(tmp, "child-session.jsonl"));

		// Closing it emits `closed` and reaps the child — no PTY exit involved.
		mgr.kill(rec.channelId);
		expect(events.some((e) => e.kind === "closed" && e.session_id === rec.sessionId)).toBe(true);
		expect(rpcChatActive(String(rec.sessionId))).toBe(false);
	} finally {
		mgr.dispose();
	}
});

test("an omp that cannot start fails the spawn instead of leaving a half-live row", async () => {
	const events: SessionEvent[] = [];
	const mgr = freshMgr(events);
	const saved = process.env.ROOST_OMP_BIN;
	// A path that resolves but cannot exec — the same observable outcome as no
	// omp at all, and machine-independent (a dev box usually HAS omp on PATH,
	// so unsetting the override would silently test nothing).
	process.env.ROOST_OMP_BIN = join(tmp, "definitely-not-omp");
	try {
		await expect(mgr.spawnAgent(tmp)).rejects.toThrow(/ROOST_OMP_BIN/);
		// No row, no `opened`: coord must never see a session whose process
		// never existed.
		expect(mgr.sessions.size).toBe(0);
		expect(events.some((e) => e.kind === "opened")).toBe(false);
	} finally {
		process.env.ROOST_OMP_BIN = saved;
		mgr.dispose();
	}
});

test("an unreadable resume path starts fresh and says so — a stale bookmark never blocks a spawn", async () => {
	const events: SessionEvent[] = [];
	const mgr = freshMgr(events);
	try {
		const missing = join(tmp, "gone.jsonl");
		const rec = await mgr.spawnAgent(tmp, { resumeSessionFile: missing });
		expect(rec.kind).toBe("agent");
		await waitFor("notice row", () => (rec.chatMessages ?? []).some((m) =>
			m.blocks.some((b) => b.kind === "text" && b.text.includes("could not read"))));
		disposeRpcChat(String(rec.sessionId));
	} finally {
		mgr.dispose();
	}
});

test("a readable, idle resume path is passed to omp as --session", async () => {
	const events: SessionEvent[] = [];
	const mgr = freshMgr(events);
	const argvLog = join(tmp, "resume.log");
	process.env.FAKE_OMP_LOG = argvLog;
	try {
		const transcript = join(tmp, "prior.jsonl");
		writeFileSync(transcript, `${JSON.stringify({ type: "session", id: "prior", cwd: tmp })}\n`);
		// Backdate past ACTIVE_WINDOW_MS: a just-written transcript reads as one a
		// live omp still owns, and resuming that is refused (next test).
		const old = new Date(Date.now() - 10 * 60_000);
		utimesSync(transcript, old, old);
		const rec = await mgr.spawnAgent(tmp, { resumeSessionFile: transcript, model: "anthropic/claude-opus-5" });
		await waitFor("child argv", () => Bun.file(argvLog).size > 0);
		const argv = (await Bun.file(argvLog).text())
			.split("\n").filter(Boolean)
			.map((l) => JSON.parse(l) as { type: string; argv?: string[] })
			.find((f) => f.type === "__argv")?.argv ?? [];
		// Paseo's order: mode, then model, then session.
		expect(argv.join(" ")).toContain("--mode rpc-ui");
		expect(argv.join(" ")).toContain(`--model anthropic/claude-opus-5 --session ${transcript}`);
		disposeRpcChat(String(rec.sessionId));
	} finally {
		process.env.FAKE_OMP_LOG = join(tmp, "child.log");
		mgr.dispose();
	}
});

test("resuming a transcript a live omp is still writing is refused, not raced", async () => {
	// Two omp processes appending to one session file corrupt it, and there is
	// no lock to consult — a recent mtime is the only evidence available. The
	// spawn must fail BEFORE anything is created.
	const events: SessionEvent[] = [];
	const mgr = freshMgr(events);
	try {
		const hot = join(tmp, "hot.jsonl");
		writeFileSync(hot, `${JSON.stringify({ type: "session", id: "hot", cwd: tmp })}\n`);
		await expect(mgr.spawnAgent(tmp, { resumeSessionFile: hot })).rejects.toThrow(/looks active/);
		expect(mgr.sessions.size).toBe(0);
		expect(events.some((e) => e.kind === "opened")).toBe(false);
	} finally {
		mgr.dispose();
	}
});
