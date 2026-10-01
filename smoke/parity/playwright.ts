// Owns the parity runner's Playwright invocations: the stack environment (the pinned Rust
// artifacts, or the TypeScript default with every stack knob removed), the project sets of the
// oracle's two passes, and the JSON report each suite pass writes under test-results/parity/.
// run.ts `spec` and `suite` call it; pass totals are read back through verdict.ts.

import { spawnSync } from "node:child_process";
import { mkdirSync, rmSync, writeFileSync } from "node:fs";
import { basename, join, relative } from "node:path";
import {
	PIN_DIRECTORY,
	REPOSITORY_ROOT,
	currentGitSha,
	formatPinManifest,
	requireCurrentPin,
	workingTreeDirty,
	type PinManifest,
} from "./pin.ts";
import {
	formatTotalsTable,
	readPassTotals,
	type ParityPass,
	type ParityStack,
	type PassRecord,
	type RunRecord,
	type SourceManifest,
} from "./verdict.ts";

const PLAYWRIGHT_CLI = "node_modules/@playwright/test/cli.js";
const PARITY_RESULTS_DIRECTORY = join(REPOSITORY_ROOT, "test-results/parity");
/** Every knob that selects a stack; a Bun run removes them so a stray shell export cannot mix stacks. */
const STACK_KNOBS = [
	"ROOST_SMOKE_COORD_EXECUTABLE",
	"ROOST_SMOKE_WORKER_EXECUTABLE",
	"ROOST_SMOKE_WEB_DIST",
	"ROOST_SMOKE_WORKER_FAULT_CONTROLS",
] as const;
const SERIAL_PASS_ARGS = ["--project=chromium-serial", "--workers=1"];

export const PARITY_PROJECTS = ["chromium-desktop", "firefox-peer", "tv", "chromium-serial"] as const;

type StackEnvironment = Record<string, string | undefined>;

export interface SpecRunOptions {
	specs: string[];
	projects: string[];
	repeat: number | undefined;
	trace: boolean;
	allowStale: boolean;
}

export interface SuiteRunOptions {
	stack: ParityStack;
	passes: ParityPass[];
	/** Names the run record; defaults to the start time. */
	label: string | undefined;
	allowStale: boolean;
}

/** Drive named specs against the pinned Rust stack with the line reporter. */
export function runSpecs(options: SpecRunOptions): number {
	const manifest = requireCurrentPin(options.allowStale);
	console.log(formatPinManifest(manifest));
	return runPlaywright([
		...options.projects.map((project) => `--project=${project}`),
		...(options.projects.includes("chromium-serial") ? ["--workers=1"] : []),
		"--reporter=line",
		...(options.repeat === undefined ? [] : ["--repeat-each", String(options.repeat)]),
		...(options.trace ? ["--trace", "on"] : []),
		...options.specs,
	], rustStackEnvironment(manifest));
}

/**
 * Run the oracle's passes on one stack and record them in `<stack>-<label>.run.json`.
 *
 * Every requested pass runs even when an earlier one failed: a red correctness pass is exactly
 * when the serial tier's numbers are still worth having. The exit is 1 when any pass failed.
 */
export function runSuite(options: SuiteRunOptions): number {
	const startedAt = new Date();
	const label = options.label ?? stamp(startedAt);
	let manifest: PinManifest | SourceManifest;
	let stackEnvironment: StackEnvironment;
	if (options.stack === "rust") {
		manifest = requireCurrentPin(options.allowStale);
		console.log(formatPinManifest(manifest));
		stackEnvironment = rustStackEnvironment(manifest);
	} else {
		manifest = { gitSha: currentGitSha(), dirty: workingTreeDirty(), ranAt: startedAt.toISOString() };
		stackEnvironment = Object.fromEntries(STACK_KNOBS.map((knob) => [knob, undefined]));
	}
	mkdirSync(PARITY_RESULTS_DIRECTORY, { recursive: true });

	const passes: PassRecord[] = [];
	try {
		if (options.stack === "bun") {
			// The same bundle `bun run test:terminal` serves: VITE_ROOST_SMOKE=1 bakes in the
			// window.__smoke chunk, and real embeds carry the keeper digest the worker admits on.
			runStep("web build", [process.execPath, "run", "--cwd", "apps/web", "build"], { VITE_ROOST_SMOKE: "1" });
			runStep("web embed", [process.execPath, "scripts/gen-embed.ts"]);
		}
		for (const pass of options.passes) passes.push(runSuitePass(options.stack, pass, stackEnvironment));
	} finally {
		if (options.stack === "bun") runStep("restore embed stubs", [process.execPath, "scripts/gen-embed.ts", "--stub"]);
	}

	const record: RunRecord = { stack: options.stack, label, manifest, passes };
	const runPath = join(PARITY_RESULTS_DIRECTORY, `${options.stack}-${label}.run.json`);
	writeFileSync(runPath, `${JSON.stringify(record, null, 2)}\n`);
	console.log([`run record: ${relative(REPOSITORY_ROOT, runPath)}`, ...formatTotalsTable(record)].join("\n"));
	return passes.every((pass) => pass.exit === 0) ? 0 : 1;
}

function runSuitePass(stack: ParityStack, pass: ParityPass, stackEnvironment: StackEnvironment): PassRecord {
	const reportPath = join(PARITY_RESULTS_DIRECTORY, `${stack}-${pass}-${stamp(new Date())}.json`);
	// A rerun inside the same minute must not read the previous run's report as its own.
	rmSync(reportPath, { force: true });
	const started = performance.now();
	const exit = runPlaywright(
		[...(pass === "main" ? correctnessProjectArgs() : SERIAL_PASS_ARGS), "--reporter=line,json"],
		{ ...stackEnvironment, PLAYWRIGHT_JSON_OUTPUT_NAME: reportPath },
	);
	const wall = (performance.now() - started) / 1000;
	const totals = readPassTotals(reportPath);
	return {
		pass,
		jsonPath: basename(reportPath),
		exit,
		passed: totals?.passed ?? null,
		failed: totals?.failed ?? null,
		skipped: totals?.skipped ?? null,
		wall,
	};
}

/** The correctness pass's projects, selected per platform exactly as apps/roost-cli/src/test.ts does. */
function correctnessProjectArgs(): string[] {
	if (process.platform === "darwin") {
		return ["--project=chromium-desktop", "--project=webkit-iphone", "--project=firefox-peer", "--project=tv"];
	}
	if (process.platform === "linux") {
		return ["--project=chromium-desktop", "--project=firefox-peer", "--project=tv"];
	}
	return ["--project=chromium-desktop", "--project=tv"];
}

/** The knobs that point the harness at the pin; fault controls only when `roost` was built with them. */
function rustStackEnvironment(manifest: PinManifest): StackEnvironment {
	const roost = join(PIN_DIRECTORY, "roost");
	return {
		ROOST_SMOKE_COORD_EXECUTABLE: roost,
		ROOST_SMOKE_WORKER_EXECUTABLE: roost,
		ROOST_SMOKE_WEB_DIST: join(PIN_DIRECTORY, "web"),
		ROOST_SMOKE_WORKER_FAULT_CONTROLS: manifest.features.includes("smoke") ? "1" : undefined,
	};
}

function runPlaywright(args: string[], stackEnvironment: StackEnvironment): number {
	const environment: Record<string, string> = {};
	const merged = { ...process.env, ROOST_TEST_BUN: process.execPath, ...stackEnvironment };
	for (const [name, value] of Object.entries(merged)) {
		if (value !== undefined) environment[name] = value;
	}
	console.log(`>> playwright test ${args.join(" ")}`);
	const result = spawnSync(
		process.execPath,
		[PLAYWRIGHT_CLI, "test", "--config=playwright.config.ts", ...args],
		{ cwd: REPOSITORY_ROOT, stdio: "inherit", env: environment },
	);
	if (result.error) throw new Error(`playwright did not start: ${String(result.error)}`);
	return result.status ?? 1;
}

function runStep(name: string, command: string[], extraEnvironment: Record<string, string> = {}): void {
	console.log(`>> ${name}`);
	const result = spawnSync(command[0]!, command.slice(1), {
		cwd: REPOSITORY_ROOT,
		stdio: "inherit",
		env: { ...process.env, ...extraEnvironment },
	});
	if (result.error) throw new Error(`${name} did not start: ${String(result.error)}`);
	if (result.status !== 0) throw new Error(`${name} failed (exit ${result.status ?? "by signal"})`);
}

/** Local `YYYYMMDD-HHMM`, the suffix of every report and default label. */
function stamp(at: Date): string {
	const twoDigits = (value: number) => String(value).padStart(2, "0");
	return `${at.getFullYear()}${twoDigits(at.getMonth() + 1)}${twoDigits(at.getDate())}-${twoDigits(at.getHours())}${twoDigits(at.getMinutes())}`;
}
