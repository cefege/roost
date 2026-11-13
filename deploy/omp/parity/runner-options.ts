import { resolve } from "node:path";

export interface RunnerShard { index: number; count: number }

export interface RunnerOptions {
  repoRoot: string;
  output?: string;
  cases: string[];
  match?: string;
  shard?: RunnerShard;
  jobs: number;
  merge: string[];
  list: boolean;
  failFast: boolean;
  keepOpen: boolean;
}

function parseShard(value: string): RunnerShard {
  const parts = /^(\d+)\/(\d+)$/.exec(value);
  const index = Number(parts?.[1]);
  const count = Number(parts?.[2]);
  if (!parts || !Number.isSafeInteger(index) || !Number.isSafeInteger(count) || count < 1 || index < 1 || index > count) {
    throw new Error(`--shard takes K/N with 1 <= K <= N, got ${value}`);
  }
  return { index, count };
}

function parseJobs(value: string): number {
  const jobs = Number(value);
  if (!/^\d+$/.test(value) || !Number.isSafeInteger(jobs) || jobs < 1 || jobs > 16) throw new Error(`--jobs takes an integer 1..16, got ${value}`);
  return jobs;
}

export function parseRunnerOptions(args: readonly string[], defaultRepoRoot: string): RunnerOptions {
  const options: RunnerOptions = { repoRoot: defaultRepoRoot, cases: [], jobs: 1, merge: [], list: false, failFast: false, keepOpen: false };
  const assigned = new Set<string>();
  for (let index = 0; index < args.length; index++) {
    const flag = args[index]!;
    if (["--list", "--fail-fast", "--keep-open"].includes(flag)) {
      if (assigned.has(flag)) throw new Error(`Duplicate argument ${flag}`);
      assigned.add(flag);
      if (flag === "--list") options.list = true;
      if (flag === "--fail-fast") options.failFast = true;
      if (flag === "--keep-open") options.keepOpen = true;
      continue;
    }
    if (!["--repo-root", "--output", "--case", "--match", "--shard", "--jobs", "--merge"].includes(flag)) throw new Error(`Unknown parity argument: ${flag}`);
    const value = args[++index];
    if (!value || value.startsWith("--")) throw new Error(`Missing value for ${flag}`);
    if (flag !== "--case" && flag !== "--merge" && assigned.has(flag)) throw new Error(`Duplicate argument ${flag}`);
    assigned.add(flag);
    switch (flag) {
      case "--repo-root": options.repoRoot = resolve(value); break;
      case "--output": options.output = resolve(value); break;
      case "--case":
        if (options.cases.includes(value)) throw new Error(`Duplicate case ${value}`);
        options.cases.push(value);
        break;
      case "--match": new RegExp(value); options.match = value; break;
      case "--shard": options.shard = parseShard(value); break;
      case "--jobs": options.jobs = parseJobs(value); break;
      case "--merge": {
        const root = resolve(value);
        if (options.merge.includes(root)) throw new Error(`Duplicate shard run ${root}`);
        options.merge.push(root);
        break;
      }
    }
  }
  const focused = options.cases.length > 0 || options.match !== undefined;
  if (options.keepOpen && (options.cases.length !== 1 || options.match !== undefined || options.list || options.shard || options.jobs !== 1)) {
    throw new Error("--keep-open requires exactly one explicit --case and no --match/--list/--shard/--jobs");
  }
  if (options.failFast && !focused) throw new Error("--fail-fast is a focused debugging option; provide --case or --match");
  // A shard is one slice of the full gate; only `--merge` of every slice may claim coverage.
  if (options.shard && focused) throw new Error("--shard partitions the full gate and cannot be combined with --case/--match");
  if (options.merge.length && (focused || options.shard || options.list || options.keepOpen || assigned.has("--jobs"))) {
    throw new Error("--merge only combines completed shard runs; it accepts no case selection or execution options");
  }
  return options;
}
