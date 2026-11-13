import "./network-guard";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadCases } from "./cases";
import { runCase, type CaseResult } from "./case-runner";
import { assertCoverage, CoverageFailure, type CoverageReport } from "./coverage";
import type { InterfaceInventory } from "./inventory-types";
import type { RunnerOptions } from "./runner-options";
import { serializeError } from "./runner-evidence";
import { exclusivePorts, runScheduled, shardSelection } from "./suite-schedule";

export interface SuiteOptions { cli: RunnerOptions; runRoot: string; sourceRoot: string; inventory: InterfaceInventory }

export async function runSuite(options: SuiteOptions): Promise<boolean> {
  const all = loadCases().all;
  for (const id of options.cli.cases) if (!all.some(item => item.scenario.id === id)) throw new Error(`Unknown parity case: ${id}`);
  const matcher = options.cli.match === undefined ? undefined : new RegExp(options.cli.match);
  const focused = options.cli.cases.length > 0 || matcher !== undefined;
  const filtered = all.filter(item => (!options.cli.cases.length || options.cli.cases.includes(item.scenario.id)) && (!matcher || matcher.test(item.scenario.id)));
  const selected = shardSelection(filtered.map((item, index) => ({ index, item })), options.cli.shard);
  if (!selected.length) throw new Error("No parity cases match the explicit selection");
  if (options.cli.keepOpen && selected.length !== 1) throw new Error("Interactive proof requires exactly one explicitly selected case");
  const mode = focused ? "focused-not-full-coverage" : options.cli.shard ? "shard" : "full";
  await writeFile(join(options.runRoot, "selection.json"), JSON.stringify({ mode, allCount: all.length, ...(options.cli.shard ? { shard: options.cli.shard } : {}),
    selected: selected.map(({ item }) => item.scenario.id), listOnly: options.cli.list }, null, 2));
  if (options.cli.list) {
    for (const { item } of selected) {
      const ports = exclusivePorts(item);
      process.stdout.write(`${item.scenario.id}\t${item.kind}\t${item.scenario.covers.join(",")}${ports.length ? `\texclusive-ports=${ports.join(",")}` : ""}\n`);
    }
    process.stdout.write("List only: no runtime cases executed and no coverage claimed.\n");
    return true;
  }
  const banner = focused ? "FOCUSED RUN — NOT FULL COVERAGE" : options.cli.shard ? `SHARD ${options.cli.shard.index}/${options.cli.shard.count} — COVERAGE REQUIRES --merge` : "FULL DIFFERENTIAL COVERAGE GATE";
  process.stdout.write(`${banner}: ${selected.length} cases, ${options.cli.jobs} concurrent\n`);
  const controller = new AbortController();
  const interrupt = (signal: NodeJS.Signals) => controller.abort(new Error(`Parity run interrupted by ${signal}`));
  const interruptInt = () => interrupt("SIGINT");
  const interruptTerm = () => interrupt("SIGTERM");
  process.on("SIGINT", interruptInt);
  process.on("SIGTERM", interruptTerm);
  let started = 0;
  let finished = 0;
  let results: CaseResult[] = [];
  try {
    results = await runScheduled(selected, options.cli.jobs, controller.signal, async ({ index, item }) => {
      process.stdout.write(`[start ${++started}/${selected.length}] ${item.scenario.id}\n`);
      const result = await runCase({ repoRoot: options.cli.repoRoot, sourceRoot: options.sourceRoot, runRoot: options.runRoot,
        index, scenario: item, signal: controller.signal, keepOpen: options.cli.keepOpen, pruneRawTraces: !focused });
      process.stdout.write(`[${++finished}/${selected.length}] ${result.passed ? "PASS" : "FAIL"} ${result.id}${result.passed ? "" : `: ${result.errors.map(error => error.message).join("; ")}`}\n`);
      return result;
    }, result => !result.passed && options.cli.failFast);
    await writeFile(join(options.runRoot, "results.json"), JSON.stringify(results, null, 2));
  } finally {
    process.off("SIGINT", interruptInt);
    process.off("SIGTERM", interruptTerm);
  }
  const complete = results.length === selected.length && results.every(result => result.passed) && !controller.signal.aborted;
  let covered = false;
  let report: CoverageReport | undefined;
  let coverageError: unknown;
  if (mode === "full") {
    try { report = assertCoverage(options.inventory, results.map(result => result.evidence)); covered = true; }
    catch (error) { coverageError = error; if (error instanceof CoverageFailure) report = error.report; }
  }
  const coverage = { mode, passed: mode === "full" && covered && complete,
    executed: results.length, selected: selected.length, ...(report ? { report } : {}),
    ...(coverageError === undefined ? {} : { error: serializeError(coverageError) }) };
  await writeFile(join(options.runRoot, "coverage.json"), JSON.stringify(coverage, null, 2));
  const passed = complete && (mode !== "full" || covered);
  await writeFile(join(options.runRoot, "summary.json"), JSON.stringify({ passed, mode, fullCoverage: mode === "full" && passed, runRoot: options.runRoot,
    executed: results.length, failed: results.filter(result => !result.passed).map(result => result.id),
    ...(controller.signal.aborted ? { interrupted: serializeError(controller.signal.reason) } : {}) }, null, 2));
  if (coverageError !== undefined) process.stderr.write(`${serializeError(coverageError).message}\n`);
  const verdict = mode === "full" ? "full runtime and independent coverage gate" : mode === "shard" ? "shard runtime result; coverage needs --merge" : "focused runtime result; NOT full coverage";
  process.stdout.write(`${passed ? "PASS" : "FAIL"}: ${verdict}. Artifacts: ${options.runRoot}\n`);
  return passed;
}
