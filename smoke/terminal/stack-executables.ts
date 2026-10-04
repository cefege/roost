// Which Rust artifacts the terminal smoke suite drives, resolved from the
// environment or the parity pin, validated, and REPORTED.
//
// Unset knobs mean the pin `bun smoke/parity/run.ts build` writes under
// `.smoke-pin/`; a knob points one side somewhere else. `smokeStackDescription()`
// names the coordinator, worker and page a run exercised, printed before the
// stack starts, because that line is the only record of which build ran.
//
// A knob that points at something missing must fail HERE, loudly, rather than
// as a hundred confusing spec failures. That is the whole reason this module
// exists instead of three `process.env` reads inline.

import { accessSync, constants, existsSync, statSync } from "node:fs";
import { isAbsolute, join, resolve } from "node:path";
import { PIN_DIRECTORY } from "../parity/pin.ts";

/** The `roost` binary whose `worker` subcommand the suite drives. */
export const SMOKE_WORKER_EXECUTABLE_ENV = "ROOST_SMOKE_WORKER_EXECUTABLE";
/** The `roost` binary whose `coord` subcommand the suite drives. */
export const SMOKE_COORD_EXECUTABLE_ENV = "ROOST_SMOKE_COORD_EXECUTABLE";
/** The dx bundle the coordinator and the worker's local door serve. */
export const SMOKE_WEB_DIST_ENV = "ROOST_SMOKE_WEB_DIST";

/** The refusal when a knob is unset and the pin it defaults to does not exist. */
export const NO_PINNED_RUST_BUILD = "smoke stack: no pinned Rust build — run `bun smoke/parity/run.ts build`";

export interface SmokeStackExecutables {
	workerExecutable: string;
	coordExecutable: string;
	/** A built SPA directory holding `index.html`. */
	webDist: string;
}

/**
 * Why a knob was refused. A message rather than a bare throw, because the
 * person who typed it wants to know which of the three is wrong.
 */
export class SmokeStackConfigurationError extends Error {
	constructor(variable: string, path: string, reason: string) {
		super(`${variable}=${path} is not usable: ${reason}`);
		this.name = "SmokeStackConfigurationError";
	}
}

/**
 * The path a knob names, resolved against `cwd`, or the pinned artifact when
 * the knob is unset.
 *
 * An empty or whitespace value counts as unset. `ROOST_SMOKE_WORKER_EXECUTABLE=`
 * is almost always a shell variable that expanded to nothing, and treating that
 * as "use the pin" is right; treating it as "run ``" is not.
 */
function knobOrPin(raw: string | undefined, cwd: string, pinned: string): string {
	const value = raw?.trim();
	if (value) return isAbsolute(value) ? value : resolve(cwd, value);
	if (!existsSync(pinned)) throw new Error(NO_PINNED_RUST_BUILD);
	return pinned;
}

/**
 * Refuse an executable that cannot be launched, at the point it is read.
 *
 * Checked for existence and for the executable bit. The executable bit is the
 * one that matters on Unix: a path that exists and is a directory, or a file
 * without `+x`, produces an EACCES deep inside a service starter where the
 * message names the service rather than the variable.
 */
function validateExecutable(variable: string, path: string): string {
	let stats;
	try {
		stats = statSync(path);
	} catch {
		throw new SmokeStackConfigurationError(variable, path, "no such file");
	}
	if (stats.isDirectory()) {
		throw new SmokeStackConfigurationError(variable, path, "it is a directory");
	}
	try {
		accessSync(path, constants.X_OK);
	} catch {
		throw new SmokeStackConfigurationError(variable, path, "it is not executable");
	}
	return path;
}

/**
 * Refuse a page directory that cannot be served, at the point it is read.
 *
 * A directory with no `index.html` is the failure worth naming: a `dx build`
 * that emitted assets but no entry point, or a path that outlived its build.
 * The coordinator launched against one answers 404s that read as a product
 * bug for the next hour instead of as a bad knob.
 */
function validateWebDirectory(variable: string, path: string): string {
	let stats;
	try {
		stats = statSync(path);
	} catch {
		throw new SmokeStackConfigurationError(variable, path, "no such directory");
	}
	if (!stats.isDirectory()) {
		throw new SmokeStackConfigurationError(variable, path, "it is not a directory");
	}
	if (!existsSync(join(path, "index.html"))) {
		throw new SmokeStackConfigurationError(variable, path, "it has no index.html");
	}
	return path;
}

/**
 * Resolve and validate every knob.
 *
 * `env`, `cwd` and `pinDirectory` are parameters so this is testable without
 * mutating the process environment or the repository's own pin.
 */
export function resolveSmokeStackExecutables(
	env: NodeJS.ProcessEnv = process.env,
	cwd: string = process.cwd(),
	pinDirectory: string = PIN_DIRECTORY,
): SmokeStackExecutables {
	const pinnedRoost = join(pinDirectory, "roost");
	return {
		workerExecutable: validateExecutable(
			SMOKE_WORKER_EXECUTABLE_ENV,
			knobOrPin(env[SMOKE_WORKER_EXECUTABLE_ENV], cwd, pinnedRoost),
		),
		coordExecutable: validateExecutable(
			SMOKE_COORD_EXECUTABLE_ENV,
			knobOrPin(env[SMOKE_COORD_EXECUTABLE_ENV], cwd, pinnedRoost),
		),
		webDist: validateWebDirectory(
			SMOKE_WEB_DIST_ENV,
			knobOrPin(env[SMOKE_WEB_DIST_ENV], cwd, join(pinDirectory, "web")),
		),
	};
}

/** A one-line description of the stack, printed before the stack starts. */
export function smokeStackDescription(stack: SmokeStackExecutables): string {
	return `smoke stack: coordinator=${stack.coordExecutable} worker=${stack.workerExecutable} web=${stack.webDist}`;
}

/**
 * Set to `1` by the parity runner when the pinned `roost` was built with the
 * `smoke` feature, whose `roost worker` accepts the two fault socket flags.
 */
const WORKER_FAULT_CONTROLS_ENV = "ROOST_SMOKE_WORKER_FAULT_CONTROLS";

/**
 * Why this run cannot drive the terminal peer fault controls, or `null` when it
 * can.
 *
 * The fault tier injects faults INTO the worker under test — a held
 * authenticated input, a blackholed packet lane, a paused history response —
 * through two disposable Unix sockets the worker connects to at boot. A
 * `roost worker` opens them only when its build has the `smoke` feature, which
 * the runner announces with `ROOST_SMOKE_WORKER_FAULT_CONTROLS=1`. A production
 * `roost worker` takes no fault argument and installs no such hook, so the
 * thirteen fault commands have nowhere to land.
 *
 * That is a gap in what this run QUALIFIES, and the honest form of a gap is a
 * named skip rather than a refusal. A stack that aborts instead turns "this
 * run cannot prove peer faults" into a dozen identical product failures whose
 * only log output is the refusal itself, which reads as a broken coordinator
 * and a broken worker when neither was ever started.
 */
export function peerFaultControlsUnavailable(
	workerExecutable: string,
	platform: NodeJS.Platform = process.platform,
): string | null {
	if (process.env[WORKER_FAULT_CONTROLS_ENV] !== "1") {
		return `terminal peer fault controls require a worker built with the smoke feature; this run drives ${workerExecutable}`;
	}
	if (platform === "win32") {
		return "terminal peer fault controls are unavailable on Windows";
	}
	return null;
}
