import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { loadCases } from "./cases";
import type { CaseResult } from "./case-runner";
import { assertCoverage, CoverageFailure, type CoverageReport } from "./coverage";
import type { InterfaceInventory } from "./inventory-types";
import { serializeError } from "./runner-evidence";

export interface MergeOptions { runRoot: string; shardRoots: string[]; inventory: InterfaceInventory; source: { revision: string; patchHashes: string[] } }

interface ShardSelection { mode: string; allCount: number; shard?: { index: number; count: number }; selected: string[]; listOnly: boolean }

async function readJson<T>(path: string): Promise<T> {
  return JSON.parse(await readFile(path, "utf8")) as T;
}

// Materialized inventory records the owning run's scratch paths; those differ
// per shard by construction and are not part of the interface identity.
function withoutRunRoot(text: string, root: string): string {
  return JSON.stringify(JSON.parse(text.split(root).join("<run-root>")));
}

function sourceIdentity(source: { revision: string; patchHashes: string[] }): string {
  return JSON.stringify({ revision: source.revision, patchHashes: source.patchHashes });
}

// Every shard must come from this exact source and inventory, and together the
// shards must execute every case exactly once; only then is coverage claimable.
export async function mergeShards(options: MergeOptions): Promise<boolean> {
  const all = loadCases().all.map(item => item.scenario.id);
  const problems: string[] = [];
  const executed = new Map<string, CaseResult>();
  const shardIndexes = new Set<number>();
  let shardCount: number | undefined;
  const expectedSource = sourceIdentity(options.source);
  const expectedInventory = withoutRunRoot(JSON.stringify(options.inventory), options.runRoot);
  for (const root of options.shardRoots) {
    const selection = await readJson<ShardSelection>(join(root, "selection.json"));
    if (selection.mode !== "shard" || !selection.shard || selection.listOnly) { problems.push(`${root}: not an executed shard run`); continue; }
    if (selection.allCount !== all.length) problems.push(`${root}: shard saw ${selection.allCount} cases, this source defines ${all.length}`);
    shardCount ??= selection.shard.count;
    if (selection.shard.count !== shardCount) problems.push(`${root}: shard count ${selection.shard.count} differs from ${shardCount}`);
    if (shardIndexes.has(selection.shard.index)) problems.push(`${root}: shard ${selection.shard.index} merged twice`);
    shardIndexes.add(selection.shard.index);
    if (sourceIdentity(await readJson<MergeOptions["source"]>(join(root, "source.json"))) !== expectedSource) problems.push(`${root}: ran a different OMP source`);
    // A shard copied off its runner keeps the run root it executed under in summary.json.
    const recordedRoot = (await readJson<{ runRoot?: string }>(join(root, "summary.json"))).runRoot ?? root;
    if (withoutRunRoot(await readFile(join(root, "inventory.json"), "utf8"), recordedRoot) !== expectedInventory) problems.push(`${root}: ran against a different inventory`);
    const results = await readJson<CaseResult[]>(join(root, "results.json"));
    const resultIds = results.map(result => result.id);
    if (JSON.stringify(resultIds) !== JSON.stringify(selection.selected)) problems.push(`${root}: executed ${resultIds.length} of ${selection.selected.length} selected cases`);
    for (const result of results) {
      if (executed.has(result.id)) problems.push(`${result.id}: executed by more than one shard`);
      executed.set(result.id, result);
    }
  }
  for (let index = 1; index <= (shardCount ?? 0); index++) if (!shardIndexes.has(index)) problems.push(`shard ${index}/${shardCount} is missing`);
  const missing = all.filter(id => !executed.has(id));
  if (missing.length) problems.push(`${missing.length} cases never executed: ${missing.slice(0, 20).join(", ")}`);
  const unknown = [...executed.keys()].filter(id => !all.includes(id));
  if (unknown.length) problems.push(`${unknown.length} executed cases are not defined by this source: ${unknown.slice(0, 20).join(", ")}`);
  const failed = [...executed.values()].filter(result => !result.passed).map(result => result.id);
  let report: CoverageReport | undefined;
  let coverageError: unknown;
  try { report = assertCoverage(options.inventory, all.flatMap(id => executed.get(id)?.evidence ?? [])); }
  catch (error) { coverageError = error; if (error instanceof CoverageFailure) report = error.report; }
  const passed = problems.length === 0 && failed.length === 0 && coverageError === undefined;
  await writeFile(join(options.runRoot, "coverage.json"), JSON.stringify({ mode: "merged-full", passed, shards: options.shardRoots,
    executed: executed.size, selected: all.length, problems, ...(report ? { report } : {}),
    ...(coverageError === undefined ? {} : { error: serializeError(coverageError) }) }, null, 2));
  await writeFile(join(options.runRoot, "summary.json"), JSON.stringify({ passed, mode: "merged-full", fullCoverage: passed, runRoot: options.runRoot,
    executed: executed.size, failed, problems }, null, 2));
  for (const problem of problems) process.stderr.write(`${problem}\n`);
  if (failed.length) process.stderr.write(`${failed.length} cases failed: ${failed.slice(0, 20).join(", ")}\n`);
  if (coverageError !== undefined) process.stderr.write(`${serializeError(coverageError).message}\n`);
  process.stdout.write(`${passed ? "PASS" : "FAIL"}: merged ${options.shardRoots.length} shards; full runtime and independent coverage gate. Artifacts: ${options.runRoot}\n`);
  return passed;
}
