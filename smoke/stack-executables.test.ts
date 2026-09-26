// The knobs the terminal smoke suite drives, both ways round: a value that
// cannot be launched fails at resolution, naming the variable, rather than as a
// hundred confusing spec failures from inside a service starter — and a value
// that resolves changes what is actually launched, which is the only thing
// that makes a mixed-stack run a test of the packaged binaries.

import { chmodSync, mkdirSync, mkdtempSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { describe, expect, test } from "bun:test";

import {
	SMOKE_COORD_EXECUTABLE_ENV,
	SMOKE_WORKER_EXECUTABLE_ENV,
	SmokeStackConfigurationError,
	coordinatorRuntimeOverrides,
	isMixedStack,
	resolveSmokeStackExecutables,
	smokeStackDescription,
} from "./terminal/stack-executables.ts";
import { listeningBind } from "./terminal/stack-coordinator.ts";
import { coordinatorLaunchPlan } from "./terminal/stack-runtime.ts";
import { cargoProfileFor, isStaleBinary } from "./terminal/stack-rust-binaries.ts";

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

// The coordinator knob has to change what is LAUNCHED, not merely what is
// reported. A knob that resolved and was then ignored would let a passing run
// be read as coverage of the Rust coordinator that never happened.
test("a coordinator knob launches that binary with the coord subcommand", () => {
	const { executable } = fixtures();
	const stack = resolveSmokeStackExecutables({ [SMOKE_COORD_EXECUTABLE_ENV]: executable }, "/tmp");
	expect(coordinatorLaunchPlan({
		bunExecutable: "/usr/bin/bun",
		...coordinatorRuntimeOverrides(stack),
	})).toEqual({ command: executable, args: ["coord"] });
	expect(smokeStackDescription(stack)).toBe(
		`smoke stack: coordinator=rust(${executable}) worker=typescript`,
	);
	expect(isMixedStack(stack)).toBe(true);
});

// The bun path must not regress: an unset knob is the TypeScript launch the
// suite has always used, entrypoint and all.
test("an unset coordinator knob launches the TypeScript entrypoint under bun", () => {
	const stack = resolveSmokeStackExecutables({}, "/tmp");
	expect(coordinatorRuntimeOverrides(stack)).toEqual({});
	expect(coordinatorLaunchPlan({
		bunExecutable: "/usr/bin/bun",
		...coordinatorRuntimeOverrides(stack),
	})).toEqual({ command: "/usr/bin/bun", args: ["apps/coord/src/main.ts"] });
});

test("both knobs set launches both binaries", () => {
	const { dir } = fixtures();
	const coord = join(dir, "roost-coord-binary");
	const worker = join(dir, "roost-worker-binary");
	for (const binary of [coord, worker]) {
		writeFileSync(binary, "#!/bin/sh\nexit 0\n");
		chmodSync(binary, 0o755);
	}
	const stack = resolveSmokeStackExecutables(
		{ [SMOKE_COORD_EXECUTABLE_ENV]: coord, [SMOKE_WORKER_EXECUTABLE_ENV]: worker },
		"/tmp",
	);
	expect(smokeStackDescription(stack)).toBe(
		`smoke stack: coordinator=rust(${coord}) worker=rust(${worker})`,
	);
});

// The startup bind is read out of the log, and the two implementations
// differ in message key and field order. A probe written for one shape must
// still read the other, or the run reports a coordinator that never boots
// while it is listening on the port it already bound.
test("the startup bind is read from either coordinator's log line", () => {
	expect(listeningBind(
		'{"ts":1,"ev":"main","level":"INFO","msg":"listening","bind":"127.0.0.1:4104"}\n',
	)).toBe("127.0.0.1:4104");
	expect(listeningBind(
		'{"bind":"127.0.0.1:53119","level":"info","message":"coordinator listening",'
		+ '"target":"roost_coord::serve","ts":1,"uptime_ms":"45"}\n',
	)).toBe("127.0.0.1:53119");
	expect(listeningBind('{"msg":"booting"}\nnot json\n')).toBeUndefined();
	expect(listeningBind('{"message":"coordinator listening"}\n')).toBeUndefined();
});

// A mixed-stack run that launches a stale binary proves an older build than the
// tree it claims to test, so the staleness decision has a test behind it.
test("a packaged binary older than the workspace is stale", () => {
	const { dir } = fixtures();
	const binary = join(dir, "target", "debug", "roost");
	const source = join(dir, "crates", "roost-coord", "lib.rs");
	mkdirSync(join(dir, "crates", "roost-coord"), { recursive: true });
	mkdirSync(join(dir, "target", "debug"), { recursive: true });
	writeFileSync(join(dir, "Cargo.toml"), "[workspace]\n");
	writeFileSync(binary, "binary\n");
	writeFileSync(source, "// source\n");
	expect(cargoProfileFor(binary, dir)).toBe("debug");
	utimesSync(join(dir, "Cargo.toml"), 1_000, 1_000);
	utimesSync(binary, 1_000, 1_000);
	utimesSync(source, 2_000, 2_000);
	expect(isStaleBinary(binary, dir)).toBe(true);
	utimesSync(binary, 3_000, 3_000);
	expect(isStaleBinary(binary, dir)).toBe(false);
});

// An installed copy is not this checkout's to rebuild, so it is used as given.
test("a binary outside the checkout target is not this harness's to build", () => {
	expect(cargoProfileFor("/usr/local/bin/roost", "/tmp/checkout")).toBeNull();
	expect(cargoProfileFor("/tmp/checkout/target/debug/roost-keeper", "/tmp/checkout")).toBeNull();
	expect(cargoProfileFor("/tmp/checkout/target/release/roost", "/tmp/checkout")).toBe("release");
});

// A coordinator knob is still VALIDATED. A typo in it must fail here rather
// than as a hundred confusing spec failures from inside a service starter.
test("an unusable coordinator knob is refused before the launcher sees it", () => {
	const { notExecutable } = fixtures();
	expect(() =>
		resolveSmokeStackExecutables({ [SMOKE_COORD_EXECUTABLE_ENV]: notExecutable }, "/tmp"),
	).toThrow(SmokeStackConfigurationError);
});
