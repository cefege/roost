// The knobs the terminal smoke suite drives, both ways round: a value that
// cannot be launched fails at resolution, naming the variable, rather than as a
// hundred confusing spec failures from inside a service starter — and an unset
// knob means the parity pin, refused by name when no pin was built.

import { chmodSync, mkdirSync, mkdtempSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

import { expect, test } from "bun:test";

import {
	NO_PINNED_RUST_BUILD,
	SMOKE_COORD_EXECUTABLE_ENV,
	SMOKE_WEB_DIST_ENV,
	SMOKE_WORKER_EXECUTABLE_ENV,
	SmokeStackConfigurationError,
	resolveSmokeStackExecutables,
	smokeStackDescription,
} from "./terminal/stack-executables.ts";
import { listeningBind } from "./terminal/stack-coordinator.ts";
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

/** A pin directory laid out the way `bun smoke/parity/run.ts build` writes one. */
function pinFixture(): { pin: string; roost: string; web: string } {
	const pin = mkdtempSync(join(tmpdir(), "roost-smoke-pin-"));
	const roost = join(pin, "roost");
	writeFileSync(roost, "#!/bin/sh\nexit 0\n");
	chmodSync(roost, 0o755);
	const web = join(pin, "web");
	mkdirSync(web);
	writeFileSync(join(web, "index.html"), "<!doctype html><title>roost</title>");
	return { pin, roost, web };
}

/** A pin directory nothing was ever built into. */
function emptyPin(): string {
	return mkdtempSync(join(tmpdir(), "roost-smoke-no-pin-"));
}

// An unparameterised run drives the pinned build on every side: the coordinator
// and the worker are the same `roost`, and the page is the pinned dx bundle.
test("an unparameterised run drives the pinned roost and page", () => {
	const { pin, roost, web } = pinFixture();
	const stack = resolveSmokeStackExecutables({}, "/tmp", pin);
	expect(stack).toEqual({ workerExecutable: roost, coordExecutable: roost, webDist: web });
	expect(smokeStackDescription(stack)).toBe(`smoke stack: coordinator=${roost} worker=${roost} web=${web}`);
});

// With no pin there is nothing to run, and the refusal says how to make one
// instead of letting a service starter fail on a missing binary.
test("an unparameterised run without a pin is refused with the build command", () => {
	expect(() => resolveSmokeStackExecutables({}, "/tmp", emptyPin())).toThrow(NO_PINNED_RUST_BUILD);
});

// Each knob replaces only its own side; the refusal still fires when any side
// falls back to a pin that does not exist.
test("a knob replaces its side and the unset sides still need the pin", () => {
	const { executable } = fixtures();
	expect(() =>
		resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: executable }, "/tmp", emptyPin()),
	).toThrow(NO_PINNED_RUST_BUILD);

	const { pin, roost } = pinFixture();
	const stack = resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: executable }, "/tmp", pin);
	expect(stack.workerExecutable).toBe(executable);
	expect(stack.coordExecutable).toBe(roost);
	expect(smokeStackDescription(stack)).toContain(`worker=${executable}`);
});

// An empty or whitespace value is a shell variable that expanded to nothing.
// Treating that as "use the pin" is right; treating it as "run ``" is not.
test("an empty knob means the pin, not an empty command", () => {
	const { pin, roost, web } = pinFixture();
	for (const value of ["", "   ", "\t"]) {
		const stack = resolveSmokeStackExecutables(
			{
				[SMOKE_WORKER_EXECUTABLE_ENV]: value,
				[SMOKE_COORD_EXECUTABLE_ENV]: value,
				[SMOKE_WEB_DIST_ENV]: value,
			},
			"/tmp",
			pin,
		);
		expect(stack).toEqual({ workerExecutable: roost, coordExecutable: roost, webDist: web });
	}
});

// A relative knob is resolved against the CWD, not left for a service starter
// to interpret from a different directory.
test("a relative knob resolves against the given directory", () => {
	const { dir, executable } = fixtures();
	const stack = resolveSmokeStackExecutables(
		{ [SMOKE_COORD_EXECUTABLE_ENV]: executable.replace(`${dir}/`, "") },
		dir,
		pinFixture().pin,
	);
	expect(stack.coordExecutable).toBe(executable);
});

// A missing path is the common typo, and the message must name the VARIABLE,
// because the person who typed it wants to know which of the three is wrong.
test("a missing executable is refused and named", () => {
	try {
		resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: "/nonexistent/x" }, "/tmp", pinFixture().pin);
		throw new Error("expected a refusal");
	} catch (error) {
		expect(error).toBeInstanceOf(SmokeStackConfigurationError);
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
		resolveSmokeStackExecutables({ [SMOKE_COORD_EXECUTABLE_ENV]: notExecutable }, "/tmp", pinFixture().pin);
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
		resolveSmokeStackExecutables({ [SMOKE_WORKER_EXECUTABLE_ENV]: dir }, "/tmp", pinFixture().pin);
		throw new Error("expected a refusal");
	} catch (error) {
		expect((error as Error).message).toContain("it is a directory");
	}
});

// A page directory that resolves must be usable as one: `index.html` is the
// entry point every browser spec opens.
test("a web dist knob is resolved when it is a built SPA directory", () => {
	const dir = mkdtempSync(join(tmpdir(), "roost-smoke-web-"));
	writeFileSync(join(dir, "index.html"), "<!doctype html><title>roost</title>");
	const stack = resolveSmokeStackExecutables({ [SMOKE_WEB_DIST_ENV]: dir }, "/tmp", pinFixture().pin);
	expect(stack.webDist).toBe(dir);
	expect(smokeStackDescription(stack)).toContain(`web=${dir}`);
});

// A directory that exists but has no entry point is the failure that reads as
// a product bug for the next hour, so it is refused here, by variable name.
test("a web dist directory without an index.html is refused and named", () => {
	const dir = mkdtempSync(join(tmpdir(), "roost-smoke-web-empty-"));
	try {
		resolveSmokeStackExecutables({ [SMOKE_WEB_DIST_ENV]: dir }, "/tmp", pinFixture().pin);
		throw new Error("expected a refusal");
	} catch (error) {
		expect(error).toBeInstanceOf(SmokeStackConfigurationError);
		expect((error as Error).message).toContain(SMOKE_WEB_DIST_ENV);
		expect((error as Error).message).toContain("no index.html");
	}
});

// The knob names a DIRECTORY; a file is a typo that the executable-bit check
// would not catch, because a served file is not a page.
test("a web dist file is refused rather than served", () => {
	const { notExecutable } = fixtures();
	try {
		resolveSmokeStackExecutables({ [SMOKE_WEB_DIST_ENV]: notExecutable }, "/tmp", pinFixture().pin);
		throw new Error("expected a refusal");
	} catch (error) {
		expect((error as Error).message).toContain("it is not a directory");
	}
});

// The startup bind is read out of the log, whose emitters differ in message
// key and field order. A probe written for one shape must still read the
// other, or the run reports a coordinator that never boots while it is
// listening on the port it already bound.
test("the startup bind is read from either log line shape", () => {
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

// A run that launches a stale binary proves an older build than the tree it
// claims to test, so the staleness decision has a test behind it.
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
