// The resolver's failure paths, which is the reason it exists: a knob that
// points at something unusable must fail at resolution, naming the variable,
// rather than as a hundred confusing spec failures deep inside a service
// starter.

import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, test } from "bun:test";

import {
	SMOKE_COORD_EXECUTABLE_ENV,
	SMOKE_WORKER_EXECUTABLE_ENV,
	SmokeStackConfigurationError,
	isMixedStack,
	resolveSmokeStackExecutables,
	smokeStackDescription,
} from "./stack-executables.ts";

/** A temp directory holding an executable and a non-executable file. */
function fixtures(): { dir: string; executable: string; notExecutable: string } {
	const dir = mkdtempSync(join(tmpdir(), "roost-smoke-stack-"));
	const executable = join(dir, "roost-worker");
	writeFileSync(executable, "#!/bin/sh\nexit 0\n");
	chmodSync(executable, 0o755);
	const notExecutable = join(dir, "notes.txt");
	writeFileSync(notExecutable, "not a program");
	return { dir, executable, notExecutable };
}

// An unparameterised run must be EXACTLY the run that always worked. If the
// default changed, every spec would need re-validating against a stack nobody
// asked for.
test("an unparameterised run uses the TypeScript stack on both sides", () => {
	const stack = resolveSmokeStackExecutables({}, "/tmp");
	expect(stack.workerExecutable).toBeNull();
	expect(stack.coordExecutable).toBeNull();
	expect(isMixedStack(stack)).toBe(false);
	expect(smokeStackDescription(stack)).toBe(
		"smoke stack: coordinator=typescript worker=typescript",
	);
});

// An empty or whitespace value is a shell variable that expanded to nothing.
// Treating that as "use the default" is right; treating it as "run ``" is not.
test("an empty knob means the default, not an empty command", () => {
	for (const value of ["", "   ", "\t"]) {
		const stack = resolveSmokeStackExecutables(
			{ [SMOKE_WORKER_EXECUTABLE_ENV]: value, [SMOKE_COORD_EXECUTABLE_ENV]: value },
			"/tmp",
		);
		expect(stack.workerExecutable).toBeNull();
		expect(stack.coordExecutable).toBeNull();
	}
});

test("an executable knob is resolved and used", () => {
	const { executable } = fixtures();
	const stack = resolveSmokeStackExecutables(
		{ [SMOKE_WORKER_EXECUTABLE_ENV]: executable },
		"/tmp",
	);
	expect(stack.workerExecutable).toBe(executable);
	expect(stack.coordExecutable).toBeNull();
	expect(isMixedStack(stack)).toBe(true);
	expect(smokeStackDescription(stack)).toContain(`worker=rust(${executable})`);
	expect(smokeStackDescription(stack)).toContain("coordinator=typescript");
});

// A relative knob is resolved against the CWD, not left for a service starter
// to interpret from a different directory.
test("a relative knob resolves against the given directory", () => {
	const { dir, executable } = fixtures();
	const stack = resolveSmokeStackExecutables(
		{ [SMOKE_COORD_EXECUTABLE_ENV]: executable.replace(`${dir}/`, "") },
		dir,
	);
	expect(stack.coordExecutable).toBe(executable);
});

// A missing path is the common typo, and the message must name the VARIABLE,
// because the person who typed it is usually mid-way through a port and wants
// to know which half is wrong.
test("a missing executable is refused and named", () => {
	expect(() =>
		resolveSmokeStackExecutables(
			{ [SMOKE_WORKER_EXECUTABLE_ENV]: "/nonexistent/roost-worker" },
			"/tmp",
		),
	).toThrow(SmokeStackConfigurationError);

	try {
		resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: "/nonexistent/x" }, "/tmp");
		throw new Error("expected a refusal");
	} catch (error) {
		expect((error as Error).message).toContain(SMOKE_WORKER_EXECUTABLE_ENV);
		expect((error as Error).message).toContain("/nonexistent/x");
		expect((error as Error).message).toContain("no such file");
	}
});

// A file without the executable bit produces an EACCES deep inside a service
// starter, where the message names the service rather than the variable.
test("a non-executable file is refused before the service starter sees it", () => {
	const { notExecutable } = fixtures();
	try {
		resolveSmokeStackExecutables(
			{ [SMOKE_COORD_EXECUTABLE_ENV]: notExecutable },
			"/tmp",
		);
		throw new Error("expected a refusal");
	} catch (error) {
		expect(error).toBeInstanceOf(SmokeStackConfigurationError);
		expect((error as Error).message).toContain(SMOKE_COORD_EXECUTABLE_ENV);
		expect((error as Error).message).toContain("not executable");
	}
});

test("a directory is refused rather than spawned", () => {
	const { dir } = fixtures();
	try {
		resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: dir }, "/tmp");
		throw new Error("expected a refusal");
	} catch (error) {
		expect((error as Error).message).toContain("it is a directory");
	}
});

// The coordinator knob is resolved and VALIDATED today but not yet launched
// from, and the description has to say so. A knob that resolves and is then
// ignored would let a passing run be read as coverage it never had.
test("a coordinator knob that is not wired yet says so in the description", () => {
	const { executable } = fixtures();
	const stack = resolveSmokeStackExecutables(
		{
			[SMOKE_WORKER_EXECUTABLE_ENV]: executable,
			[SMOKE_COORD_EXECUTABLE_ENV]: executable,
		},
		"/tmp",
	);
	expect(stack.coordExecutable).toBe(executable);
	expect(smokeStackDescription(stack)).toContain("NOT wired");
	expect(smokeStackDescription(stack)).toContain(SMOKE_COORD_EXECUTABLE_ENV);
	expect(smokeStackDescription(stack)).not.toContain(`coordinator=rust(${executable})`);
	expect(isMixedStack(stack)).toBe(true);
});

// An unwired knob is still VALIDATED. A typo in it should fail here, not
// silently do nothing.
test("an unwired coordinator knob is still refused when it is unusable", () => {
	const { notExecutable } = fixtures();
	expect(() =>
		resolveSmokeStackExecutables({ [SMOKE_COORD_EXECUTABLE_ENV]: notExecutable }, "/tmp"),
	).toThrow(SmokeStackConfigurationError);
});
