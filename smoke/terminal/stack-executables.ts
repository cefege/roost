// Which executables the terminal smoke suite drives, resolved from the
// environment, validated, and REPORTED.
//
// The suite is the same 140 specs either way; what changes is the stack under
// them. The default is the TypeScript stack, so an unparameterised run is
// exactly the run that always worked, and a mixed-stack run is opt-in and
// self-identifying: `smokeStackDescription()` says which coordinator and which
// worker this run exercises, and that line is printed before the stack starts.
//
// A knob that points at something missing must fail HERE, loudly, rather than
// as a hundred confusing spec failures. That is the whole reason this module
// exists instead of two `process.env` reads inline.

import { accessSync, constants, statSync } from "node:fs";
import { isAbsolute, resolve } from "node:path";

/** The worker binary the suite drives. Unset means the TypeScript worker. */
export const SMOKE_WORKER_EXECUTABLE_ENV = "ROOST_SMOKE_WORKER_EXECUTABLE";
/** The coordinator binary the suite drives. Unset means the TypeScript one. */
export const SMOKE_COORD_EXECUTABLE_ENV = "ROOST_SMOKE_COORD_EXECUTABLE";

export interface SmokeStackExecutables {
	/** The worker binary, or `null` for the TypeScript worker. */
	workerExecutable: string | null;
	/** The coordinator binary, or `null` for the TypeScript coordinator. */
	coordExecutable: string | null;
}

/**
 * Why a knob was refused. A message rather than a bare throw, because the
 * person who typed it is usually mid-way through a port and wants to know which
 * half is wrong.
 */
export class SmokeStackConfigurationError extends Error {
	constructor(variable: string, path: string, reason: string) {
		super(`${variable}=${path} is not usable: ${reason}`);
		this.name = "SmokeStackConfigurationError";
	}
}

/**
 * Resolve a knob to an absolute path, or `null` when it is unset.
 *
 * An empty or whitespace value counts as unset. `ROOST_SMOKE_WORKER_EXECUTABLE=`
 * is almost always a shell variable that expanded to nothing, and treating that
 * as "use the default" is right; treating it as "run ``" is not.
 */
function resolveOptional(
	variable: string,
	raw: string | undefined,
	cwd: string,
): string | null {
	const value = raw?.trim();
	if (!value) return null;
	return isAbsolute(value) ? value : resolve(cwd, value);
}

/**
 * Refuse a knob that cannot be launched, at the point it is read.
 *
 * Checked for existence and for the executable bit. The executable bit is the
 * one that matters on Unix: a path that exists and is a directory, or a file
 * without `+x`, produces an EACCES deep inside a service starter where the
 * message names the service rather than the variable.
 */
function validate(variable: string, path: string): string {
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
 * Resolve and validate both knobs.
 *
 * `env` and `cwd` are parameters so this is testable without mutating the
 * process environment, which is the only way to test the failure paths.
 */
export function resolveSmokeStackExecutables(
	env: NodeJS.ProcessEnv = process.env,
	cwd: string = process.cwd(),
): SmokeStackExecutables {
	const worker = resolveOptional(SMOKE_WORKER_EXECUTABLE_ENV, env[SMOKE_WORKER_EXECUTABLE_ENV], cwd);
	const coord = resolveOptional(SMOKE_COORD_EXECUTABLE_ENV, env[SMOKE_COORD_EXECUTABLE_ENV], cwd);
	return {
		workerExecutable: worker ? validate(SMOKE_WORKER_EXECUTABLE_ENV, worker) : null,
		coordExecutable: coord ? validate(SMOKE_COORD_EXECUTABLE_ENV, coord) : null,
	};
}

/** Whether this run drives a RUST binary on either side. */
export function isMixedStack(stack: SmokeStackExecutables): boolean {
	return stack.workerExecutable !== null || stack.coordExecutable !== null;
}

/**
 * A one-line description of the stack, printed before the stack starts.
 *
 * This is what makes a mixed-stack run legible in CI output: a failing spec
 * that ran against a Rust coordinator and a Rust worker is a different bug
 * from one that ran against the TypeScript pair, and the log line is the only
 * place that distinction is recorded.
 */
export function smokeStackDescription(stack: SmokeStackExecutables): string {
	const worker = stack.workerExecutable ? `rust(${stack.workerExecutable})` : "typescript";
	const coord = stack.coordExecutable ? `rust(${stack.coordExecutable})` : "typescript";
	return `smoke stack: coordinator=${coord} worker=${worker}`;
}

/**
 * The coordinator launch overrides implied by the environment.
 *
 * A packaged binary REPLACES the TypeScript entrypoint and receives the
 * ordinary `coord` subcommand, the same rule the worker side follows. An
 * empty object for the default keeps the caller's spread a no-op rather than
 * a branch.
 */
export function coordinatorRuntimeOverrides(stack: SmokeStackExecutables): {
	coordExecutable?: string;
} {
	if (!stack.coordExecutable) return {};
	return { coordExecutable: stack.coordExecutable };
}

/**
 * The worker runtime overrides implied by the environment.
 *
 * A packaged binary REPLACES the TypeScript entrypoint, and the two may not be
 * combined — `createTerminalWorkerStarter` already refuses that pairing, and
 * repeating the rule here keeps the reason next to the decision. Returning an
 * empty object for the default keeps the caller's spread a no-op rather than a
 * branch.
 */
export function workerRuntimeOverrides(stack: SmokeStackExecutables): {
	workerExecutable?: string;
	sourceEntrypoint?: undefined;
} {
	if (!stack.workerExecutable) return {};
	return { workerExecutable: stack.workerExecutable, sourceEntrypoint: undefined };
}
