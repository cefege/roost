// Reads the Playwright JSON reports a parity suite wrote and buckets every test by its Rust and
// Bun outcome; run.ts `suite` records pass totals through it and `verdict` prints its table.
// A test is keyed by (file, title path, project), never by line, which an oracle edit moves; a
// pass whose report is missing is reported as crashed rather than counted as zero tests.

import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { basename, dirname, join, resolve } from "node:path";
import { z } from "zod";
import { PinManifestSchema, formatPinManifest } from "./pin.ts";

export type ParityStack = "rust" | "bun";
export type ParityPass = "main" | "serial";

/** What a `bun` run records in place of a pin: the source tree the TypeScript stack ran from. */
const SourceManifestSchema = z.object({ gitSha: z.string(), dirty: z.boolean(), ranAt: z.string() });
export type SourceManifest = z.infer<typeof SourceManifestSchema>;

const PassRecordSchema = z.object({
	pass: z.enum(["main", "serial"]),
	// The pass's Playwright JSON report, relative to the `.run.json` that records it.
	jsonPath: z.string(),
	exit: z.number(),
	passed: z.number().nullable(),
	failed: z.number().nullable(),
	skipped: z.number().nullable(),
	// Wall-clock seconds the pass took.
	wall: z.number(),
});
export type PassRecord = z.infer<typeof PassRecordSchema>;

/** The `test-results/parity/<stack>-<label>.run.json` a suite writes. */
const RunRecordSchema = z.object({
	stack: z.enum(["rust", "bun"]),
	label: z.string(),
	manifest: z.union([PinManifestSchema, SourceManifestSchema]),
	passes: z.array(PassRecordSchema),
});
export type RunRecord = z.infer<typeof RunRecordSchema>;

type OutcomeStatus = "passed" | "failed" | "timedOut" | "interrupted" | "skipped";

interface TestOutcome {
	file: string;
	line: number;
	title: string;
	project: string;
	status: OutcomeStatus;
	/** The first error line of a red test, the skip reason of a skipped one. */
	detail: string;
}

// The subset of the Playwright JSON reporter's output this module reads.
const ReportSpecSchema = z.object({
	title: z.string(),
	file: z.string(),
	line: z.number(),
	tests: z.array(z.object({
		projectName: z.string(),
		status: z.string(),
		annotations: z.array(z.object({ type: z.string(), description: z.string().optional() })).optional(),
		results: z.array(z.object({
			status: z.enum(["passed", "failed", "timedOut", "interrupted", "skipped"]),
			errors: z.array(z.object({ message: z.string().optional() })).optional(),
			error: z.object({ message: z.string().optional() }).optional(),
		})),
	})),
});
type ReportTest = z.infer<typeof ReportSpecSchema>["tests"][number];
interface ReportSuite {
	title: string;
	specs?: z.infer<typeof ReportSpecSchema>[];
	suites?: ReportSuite[];
}
const ReportSuiteSchema: z.ZodType<ReportSuite> = z.lazy(() =>
	z.object({
		title: z.string(),
		specs: z.array(ReportSpecSchema).optional(),
		suites: z.array(ReportSuiteSchema).optional(),
	})
);
const ReportSchema = z.object({
	suites: z.array(ReportSuiteSchema).optional(),
	stats: z.object({ expected: z.number(), unexpected: z.number(), skipped: z.number(), flaky: z.number() }).optional(),
});
type Report = z.infer<typeof ReportSchema>;

const BUCKETS = ["gap", "both-red", "rust-skip-only", "both-skip", "green", "bun-only", "skew"] as const;
type Bucket = (typeof BUCKETS)[number];
const BUCKET_MEANING: Record<Bucket, string> = {
	gap: "Rust red, Bun green: a parity gap",
	"both-red": "red on both stacks",
	"rust-skip-only": "Rust skipped, Bun green: a capability Rust lacks",
	"both-skip": "skipped on both stacks",
	green: "Rust green",
	"bun-only": "collected on Bun only",
	skew: "any other pairing: Rust red where Bun skipped, Rust skipped where Bun is red, or Rust only",
};
const ANSI_ESCAPE = /\u001b\[[0-9;]*[A-Za-z]/g;

/** A pass report's totals, or `null` when the pass wrote no readable report. */
export function readPassTotals(jsonPath: string): { passed: number; failed: number; skipped: number } | null {
	const stats = readReport(jsonPath)?.stats;
	if (stats === undefined) return null;
	return { passed: stats.expected + stats.flaky, failed: stats.unexpected, skipped: stats.skipped };
}

/** The totals table `suite` and a one-run `verdict` print. */
export function formatTotalsTable(record: RunRecord): string[] {
	const rows = [["stack", "pass", "exit", "passed", "failed", "skipped", "wall"]];
	for (const pass of record.passes) {
		rows.push([
			record.stack,
			pass.pass,
			String(pass.exit),
			pass.passed === null ? "crashed" : String(pass.passed),
			pass.failed === null ? "-" : String(pass.failed),
			pass.skipped === null ? "-" : String(pass.skipped),
			`${Math.round(pass.wall)}s`,
		]);
	}
	const widths = rows[0]!.map((_, column) => Math.max(...rows.map((row) => row[column]!.length)));
	return rows.map((row) => row.map((cell, column) => cell.padEnd(widths[column]!)).join("  ").trimEnd());
}

/** Print one run's manifest, totals and red tests; exit 1 when anything is red or crashed. */
export function summarizeRun(runPath: string): number {
	const run = loadRun(runPath);
	const red = [...run.outcomes.values()].filter((outcome) => isRed(outcome.status));
	const lines = [
		`${run.record.stack}: ${describeManifest(run.record)}`,
		...formatTotalsTable(run.record),
		...run.crashed,
		"",
		`## red — ${red.length}`,
		...red.sort(byLocation).map((outcome) => `- ${formatOutcome(outcome, outcome.detail)}`),
	];
	console.log(lines.join("\n"));
	return run.crashed.length > 0 || red.length > 0 ? 1 : 0;
}

/**
 * Bucket every test of a Rust run against a Bun run and print the worklist.
 *
 * Exits 1 when a gap or a Rust-only skip remains, when a skew is red on Rust, or when either run
 * has a crashed pass: each of those is a verdict the table could not honestly call parity.
 */
export function compareRuns(rustRunPath: string, bunRunPath: string, markdownPath: string | undefined): number {
	const rust = loadRun(rustRunPath);
	const bun = loadRun(bunRunPath);
	const buckets = Object.fromEntries(BUCKETS.map((bucket) => [bucket, []])) as unknown as Record<
		Bucket,
		{ outcome: TestOutcome; detail: string }[]
	>;
	let redSkews = 0;
	for (const key of new Set([...rust.outcomes.keys(), ...bun.outcomes.keys()])) {
		const rustOutcome = rust.outcomes.get(key);
		const bunOutcome = bun.outcomes.get(key);
		const bucket = bucketOf(rustOutcome, bunOutcome);
		const shown = (rustOutcome ?? bunOutcome)!;
		const detail = bucket === "skew"
			? `rust ${rustOutcome?.status ?? "absent"} / bun ${bunOutcome?.status ?? "absent"} — ${shown.detail}`
			: shown.detail;
		if (bucket === "skew" && rustOutcome !== undefined && isRed(rustOutcome.status)) redSkews += 1;
		buckets[bucket].push({ outcome: shown, detail });
	}
	const lines = [
		`rust: ${describeManifest(rust.record)}`,
		`bun:  ${describeManifest(bun.record)}`,
		...rust.crashed,
		...bun.crashed,
		"",
		...BUCKETS.map((bucket) => `${bucket.padEnd(15)} ${String(buckets[bucket].length).padStart(4)}`),
	];
	for (const bucket of BUCKETS) {
		const entries = buckets[bucket].sort((left, right) => byLocation(left.outcome, right.outcome));
		lines.push("", `## ${bucket} — ${entries.length} (${BUCKET_MEANING[bucket]})`, "");
		lines.push(...entries.map(({ outcome, detail }) => `- ${formatOutcome(outcome, detail)}`));
	}
	const text = `${lines.join("\n")}\n`;
	console.log(text);
	if (markdownPath !== undefined) writeFileSync(markdownPath, `# Parity verdict\n\n${text}`);
	const unparity = buckets.gap.length + buckets["rust-skip-only"].length + redSkews;
	return unparity > 0 || rust.crashed.length > 0 || bun.crashed.length > 0 ? 1 : 0;
}

interface LoadedRun {
	record: RunRecord;
	outcomes: Map<string, TestOutcome>;
	crashed: string[];
}

function loadRun(runPath: string): LoadedRun {
	const record = RunRecordSchema.parse(JSON.parse(readFileSync(runPath, "utf8")));
	const outcomes = new Map<string, TestOutcome>();
	const crashed: string[] = [];
	for (const pass of record.passes) {
		const reportPath = passReportPath(runPath, pass);
		const report = readReport(reportPath);
		if (report === null) {
			crashed.push(`${record.stack} ${pass.pass}: pass crashed (exit ${pass.exit}; no report at ${reportPath})`);
			continue;
		}
		const passOutcomes: TestOutcome[] = [];
		for (const suite of report.suites ?? []) collectOutcomes(suite, [], passOutcomes);
		for (const outcome of passOutcomes) {
			// A repeated spec keeps its worst result: one red repetition is a red test.
			const key = [outcome.file, outcome.title, outcome.project].join("\u0000");
			const prior = outcomes.get(key);
			if (prior === undefined || severity(outcome.status) > severity(prior.status)) outcomes.set(key, outcome);
		}
	}
	return { record, outcomes, crashed };
}

/**
 * Where a pass report lives: `jsonPath` beside the run record, or, for an archived copy that
 * renamed its reports, `<stem>-<pass>.json` beside `<stem>.run.json`.
 */
function passReportPath(runPath: string, pass: PassRecord): string {
	const recorded = resolve(dirname(runPath), pass.jsonPath);
	if (existsSync(recorded)) return recorded;
	return join(dirname(runPath), `${basename(runPath, ".run.json")}-${pass.pass}.json`);
}

function readReport(jsonPath: string): Report | null {
	if (!existsSync(jsonPath)) return null;
	let parsed: unknown;
	try {
		parsed = JSON.parse(readFileSync(jsonPath, "utf8"));
	} catch {
		// A report cut off mid-write is the same verdict as no report: the pass did not finish.
		return null;
	}
	// A complete report of another shape is a reporter change this table must not misread.
	return ReportSchema.parse(parsed);
}

/** The file-level suite's title is the file; nested suites are describe blocks. */
function collectOutcomes(suite: ReportSuite, describePath: string[], into: TestOutcome[]): void {
	for (const spec of suite.specs ?? []) {
		const title = [...describePath, spec.title].join(" › ");
		for (const test of spec.tests) {
			const status = normalizedStatus(test);
			const last = test.results.at(-1);
			const errorText = (last?.errors?.[0]?.message ?? last?.error?.message ?? "").replace(ANSI_ESCAPE, "");
			const detail = status === "skipped"
				? (test.annotations?.find((annotation) => annotation.type === "skip")?.description ?? "")
				: (errorText.split("\n").map((line) => line.trim()).find((line) => line !== "") ?? "");
			into.push({ file: spec.file, line: spec.line, title, project: test.projectName, status, detail });
		}
	}
	for (const child of suite.suites ?? []) collectOutcomes(child, [...describePath, child.title], into);
}

/** The last result decides, read through the test's expected/unexpected verdict. */
function normalizedStatus(test: ReportTest): OutcomeStatus {
	const last = test.results.at(-1);
	if (last === undefined) return test.status === "skipped" ? "skipped" : "interrupted";
	if (last.status === "skipped") return "skipped";
	if (test.status === "expected" || test.status === "flaky") return "passed";
	return last.status === "passed" ? "failed" : last.status;
}

function bucketOf(rust: TestOutcome | undefined, bun: TestOutcome | undefined): Bucket {
	if (rust === undefined) return "bun-only";
	if (rust.status === "passed") return "green";
	if (bun === undefined) return "skew";
	if (isRed(rust.status) && bun.status === "passed") return "gap";
	if (isRed(rust.status) && isRed(bun.status)) return "both-red";
	if (rust.status === "skipped" && bun.status === "passed") return "rust-skip-only";
	if (rust.status === "skipped" && bun.status === "skipped") return "both-skip";
	return "skew";
}

function isRed(status: OutcomeStatus): boolean {
	return status !== "passed" && status !== "skipped";
}

function severity(status: OutcomeStatus): number {
	if (isRed(status)) return 2;
	return status === "skipped" ? 1 : 0;
}

function formatOutcome(outcome: TestOutcome, detail: string): string {
	return `${outcome.file}:${outcome.line} — ${outcome.title} [${outcome.project}]${detail === "" ? "" : ` — ${detail}`}`;
}

function byLocation(left: TestOutcome, right: TestOutcome): number {
	return left.file.localeCompare(right.file) || left.line - right.line || left.project.localeCompare(right.project);
}

function describeManifest(record: RunRecord): string {
	if ("roostSha256" in record.manifest) return `${record.label} ${formatPinManifest(record.manifest)}`;
	const source = record.manifest;
	return `${record.label} source ${source.gitSha.slice(0, 12)}${source.dirty ? "+dirty" : ""} ran=${source.ranAt}`;
}
