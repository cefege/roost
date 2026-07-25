// SessionsSpawn kind → worker control frame.
//
// This mapping used to be a ternary: anything that was not "shell" became
// spawn-claude. With a third kind on the wire that is not a defaulting choice,
// it is a silent wrong-session bug — an `agent` request would come back as a
// claude PTY. Three explicit cases, and an unknown kind is a loud
// InvalidArgument rather than a guess.

import { test, expect } from "bun:test";
import { ConnectError, Code } from "@connectrpc/connect";
import { _spawnFrameFor } from "../src/connect/handlers-sessions.ts";

test("shell and claude keep their PTY dims", () => {
	expect(_spawnFrameFor({ kind: "shell", folder: "/tmp", cols: 100, rows: 30 }))
		.toEqual({ kind: "spawn-shell", folder: "/tmp", cols: 100, rows: 30 });
	expect(_spawnFrameFor({ kind: "claude", folder: "/tmp", cols: 100, rows: 30, initialMode: "plan" }))
		.toEqual({ kind: "spawn-claude", folder: "/tmp", initial_mode: "plan", cols: 100, rows: 30 });
});

test("agent carries the resume path and model, and NO cols/rows — it has no PTY", () => {
	const frame = _spawnFrameFor({
		kind: "agent", folder: "/work", cols: 100, rows: 30,
		resumeSessionFile: "/home/u/.omp/agent/sessions/a.jsonl", model: "anthropic/claude-opus-5",
	});
	expect(frame).toEqual({
		kind: "spawn-agent", folder: "/work",
		resume_session_file: "/home/u/.omp/agent/sessions/a.jsonl",
		model: "anthropic/claude-opus-5",
	});
	// Omitted resume/model default to empty, not undefined: the worker schema
	// defaults them and "" means "fresh conversation / omp's own default".
	expect(_spawnFrameFor({ kind: "agent", folder: "/work" }))
		.toEqual({ kind: "spawn-agent", folder: "/work", resume_session_file: "", model: "" });
});

test("a caller-minted session id rides every kind", () => {
	const sessionId = "11111111-1111-1111-1111-111111111111";
	for (const kind of ["shell", "claude", "agent"]) {
		expect(_spawnFrameFor({ kind, folder: "/tmp", sessionId })).toMatchObject({ session_id: sessionId });
	}
});

test("an unknown kind is refused, not defaulted", () => {
	try {
		_spawnFrameFor({ kind: "wat", folder: "/tmp" });
		throw new Error("expected a throw");
	} catch (e) {
		expect(e).toBeInstanceOf(ConnectError);
		expect((e as ConnectError).code).toBe(Code.InvalidArgument);
	}
});
