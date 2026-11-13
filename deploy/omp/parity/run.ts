import { mkdir, mkdtemp, realpath, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { prepareSource } from "./source";
import { readInventory } from "./inventory";
import { createHome, isolatedEnvironment } from "./runtime";
import { parseRunnerOptions } from "./runner-options";
import { sourceLoader } from "./source-load";

const cli = parseRunnerOptions(process.argv.slice(2), fileURLToPath(new URL("../../../", import.meta.url)));
cli.repoRoot = await realpath(cli.repoRoot);
const output = cli.output ?? join(cli.repoRoot, "target/omp-parity/runs");
await mkdir(output, { recursive: true });
const runRoot = await mkdtemp(join(await realpath(output), "run-"));
process.stdout.write(`OMP_PARITY_RUN_ROOT ${runRoot}\n`);
try {
  const source = await prepareSource(cli.repoRoot);
  const inventory = await readInventory(source.root);
  await mkdir(join(cli.repoRoot, "target/omp-parity"), { recursive: true });
  await writeFile(join(runRoot, "source.json"), `${JSON.stringify(source, null, 2)}\n`);
  const home = join(runRoot, "runner-home");
  await createHome(home);
  const networkTrace = join(home, "metadata-network.jsonl");
  await writeFile(networkTrace, "");
  const environment = isolatedEnvironment(home, { OMP_PARITY_SOURCE_ROOT: source.root,
    PLAYWRIGHT_BROWSERS_PATH: join(cli.repoRoot, "target/omp-parity/browsers"),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: join(cli.repoRoot, "target/omp-parity/bun-transpiler-cache"),
    OMP_PARITY_INVENTORY: join(cli.repoRoot, "target/omp-parity/inventory.json"),
    OMP_PARITY_NETWORK_TRACE: networkTrace, OMP_PARITY_ALLOWED_ORIGINS: "" });
  for (const key of Object.keys(process.env)) delete process.env[key];
  Object.assign(process.env, environment);
  process.chdir(home);
  Bun.plugin({ name: "owned-omp-source-alias", setup(build) {
    build.onResolve({ filter: /^@omp-source\// }, args => {
      const relative = args.path.slice("@omp-source/".length);
      if (!relative.startsWith("packages/") || relative.split("/").includes("..")) throw new Error(`Unowned OMP source alias: ${args.path}`);
      return { path: Bun.resolveSync(resolve(source.root, relative), source.root) };
    });
  } });
  await import("./network-guard");
  const inventoryWorkspace = join(home, "inventory-workspace");
  await mkdir(inventoryWorkspace, { recursive: true });
  const { materializeSettingOptions } = await import("./inventory-runtime-settings");
  await materializeSettingOptions(inventory, { sourceRoot: source.root, workspace: inventoryWorkspace, load: sourceLoader(source.root) });
  await writeFile(join(cli.repoRoot, "target/omp-parity/inventory.json"), `${JSON.stringify(inventory, null, 2)}\n`);
  await writeFile(join(runRoot, "inventory.json"), `${JSON.stringify(inventory, null, 2)}\n`);
  // Actual-source metadata may initialize Settings: import only after replacing HOME and installing the deny-all network boundary.
  if (cli.merge.length) {
    const { mergeShards } = await import("./suite-merge");
    if (!await mergeShards({ runRoot, shardRoots: cli.merge, inventory, source })) process.exitCode = 1;
  } else {
    const { runSuite } = await import("./suite");
    if (!await runSuite({ cli, runRoot, sourceRoot: source.root, inventory })) process.exitCode = 1;
  }
} catch (error) {
  const describe = (value: unknown): object => value instanceof Error
    ? { name: value.name, message: value.message, stack: value.stack,
      ...(value.cause === undefined ? {} : { cause: describe(value.cause) }),
      ...(value instanceof AggregateError ? { errors: [...value.errors].map(describe) } : {}) }
    : { message: String(value) };
  if (!await Bun.file(join(runRoot, "coverage.json")).exists()) {
    await writeFile(join(runRoot, "coverage.json"), JSON.stringify({ passed: false, phase: "bootstrap-or-suite", error: describe(error) }, null, 2));
  }
  await writeFile(join(runRoot, "bootstrap-or-suite-error.json"), JSON.stringify(describe(error), null, 2));
  process.stderr.write(`${error instanceof Error ? error.stack ?? error.message : String(error)}\nArtifacts: ${runRoot}\n`);
  process.exitCode = 1;
}
