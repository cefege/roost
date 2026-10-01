// The one entry point for building and running the terminal oracle against the Rust stack:
// `build` pins the artifacts, `spec` and `suite` drive Playwright against the pin (or the
// TypeScript default), and `verdict` buckets a Rust run against a Bun run. The modules beside
// it own each step; CLAUDE.md `### Commands` names the loop this serves.

import { parseArgs } from "node:util";
import { ParityRefusal, buildAndPinArtifacts, formatPinManifest } from "./pin.ts";
import { PARITY_PROJECTS, runSpecs, runSuite } from "./playwright.ts";
import { compareRuns, summarizeRun, type ParityPass } from "./verdict.ts";

const USAGE = `usage: bun smoke/parity/run.ts <command>
  build [--no-web] [--plain]
  spec <path[:line]>... [--project <name>]... [--repeat <n>] [--trace] [--allow-stale]
  suite --stack rust|bun [--pass main|serial|both] [--label <text>] [--allow-stale]
  verdict <rust.run.json> [<bun.run.json> [--md <out.md>]]`;

function dispatchCommand(argv: string[]): number {
	const [command, ...rest] = argv;
	switch (command) {
		case "build": {
			const { values } = parseArgs({
				args: rest,
				strict: true,
				options: { "no-web": { type: "boolean" }, plain: { type: "boolean" } },
			});
			const manifest = buildAndPinArtifacts({ buildWeb: values["no-web"] !== true, plain: values.plain === true });
			console.log(JSON.stringify(manifest, null, 2));
			console.log(formatPinManifest(manifest));
			return 0;
		}
		case "spec": {
			const { values, positionals } = parseArgs({
				args: rest,
				strict: true,
				allowPositionals: true,
				options: {
					project: { type: "string", multiple: true },
					repeat: { type: "string" },
					trace: { type: "boolean" },
					"allow-stale": { type: "boolean" },
				},
			});
			if (positionals.length === 0) throw new ParityRefusal(USAGE);
			const projects = values.project ?? [];
			const unknown = projects.filter((project) => !(PARITY_PROJECTS as readonly string[]).includes(project));
			if (unknown.length > 0) {
				throw new ParityRefusal(`unknown project ${unknown.join(", ")}; one of ${PARITY_PROJECTS.join(", ")}`);
			}
			const repeat = values.repeat === undefined ? undefined : Number(values.repeat);
			if (repeat !== undefined && !(Number.isInteger(repeat) && repeat > 0)) {
				throw new ParityRefusal(`--repeat takes a positive integer, not ${values.repeat}`);
			}
			return runSpecs({
				specs: positionals,
				projects,
				repeat,
				trace: values.trace === true,
				allowStale: values["allow-stale"] === true,
			});
		}
		case "suite": {
			const { values } = parseArgs({
				args: rest,
				strict: true,
				options: {
					stack: { type: "string" },
					pass: { type: "string", default: "both" },
					label: { type: "string" },
					"allow-stale": { type: "boolean" },
				},
			});
			const stack = values.stack;
			if (stack !== "rust" && stack !== "bun") throw new ParityRefusal(USAGE);
			const passes: ParityPass[] | null = values.pass === "both"
				? ["main", "serial"]
				: values.pass === "main" || values.pass === "serial" ? [values.pass] : null;
			if (passes === null) throw new ParityRefusal(USAGE);
			if (values.label !== undefined && !/^[\w.-]+$/.test(values.label)) {
				throw new ParityRefusal(`--label names a file; use letters, digits, '.', '_' or '-', not ${values.label}`);
			}
			return runSuite({ stack, passes, label: values.label, allowStale: values["allow-stale"] === true });
		}
		case "verdict": {
			const { values, positionals } = parseArgs({
				args: rest,
				strict: true,
				allowPositionals: true,
				options: { md: { type: "string" } },
			});
			if (positionals.length === 1 && values.md === undefined) return summarizeRun(positionals[0]!);
			if (positionals.length === 2) return compareRuns(positionals[0]!, positionals[1]!, values.md);
			throw new ParityRefusal(USAGE);
		}
		default:
			throw new ParityRefusal(USAGE);
	}
}

try {
	process.exit(dispatchCommand(process.argv.slice(2)));
} catch (error) {
	// node:util parseArgs reports an unknown or malformed option as ERR_PARSE_ARGS_*: a usage error.
	const argumentError = typeof error === "object" && error !== null && "code" in error
		&& typeof error.code === "string" && error.code.startsWith("ERR_PARSE_ARGS");
	const usageError = error instanceof ParityRefusal || argumentError;
	console.error(error instanceof Error ? error.message : String(error));
	process.exit(usageError ? 2 : 1);
}
