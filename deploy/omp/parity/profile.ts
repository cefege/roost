import { chmod, mkdir, realpath, writeFile } from "node:fs/promises";
import { delimiter, join, resolve } from "node:path";
import type { ParityCase } from "./cases";
import { createHome, execute, isolatedEnvironment } from "./runtime";
import { startProvider, type ProviderSession } from "./provider";
import { startBuiltinServices, type RunningBuiltinServices } from "./fixture-services";
import { readBuiltinSeedManifest } from "./builtin-seed";
import { assertOwned, profileManifestPath, type ProfileManifest } from "./profile-manifest";
import { sourceLoader } from "./source-load";
import { stopProfileRuntime } from "./profile-process";

export interface LegProfile {
  home: string;
  workspace: string;
  environment: Record<string, string>;
  command: string[];
  effectsPath: string;
  probeTracePath: string;
  provider: ProviderSession;
  stop(): Promise<void>;
}

const shellQuote = (value: string): string => `'${value.replaceAll("'", "'\\''")}'`;

export async function createLauncher(repoRoot: string, sourceRoot: string, runRoot: string): Promise<string> {
  const repository = await realpath(repoRoot);
  const source = await realpath(sourceRoot);
  if (source !== await realpath(join(repository, "target/omp-parity-source"))) throw new Error("Parity launcher accepts only the owned pinned source checkout");
  const root = resolve(runRoot);
  const bin = join(root, ".parity-bin");
  await mkdir(bin, { recursive: true });
  const executable = join(bin, "omp");
  await writeFile(executable, `#!/bin/sh\nexec ${shellQuote(process.execPath)} ${shellQuote(join(repository, "deploy/omp/parity/launch.ts"))} --repo-root ${shellQuote(repository)} --source-root ${shellQuote(source)} --run-root ${shellQuote(root)} -- "$@"\n`);
  await chmod(executable, 0o700);
  return executable;
}

export async function createLegProfile(options: { repoRoot: string; sourceRoot: string; workspace: string; scenario: ParityCase; adapter: "terminal" | "browser" }): Promise<LegProfile> {
  const repoRoot = await realpath(options.repoRoot);
  const sourceRoot = await realpath(options.sourceRoot);
  const workspace = resolve(options.workspace);
  await mkdir(workspace, { recursive: true });
  if (await Bun.file(profileManifestPath(workspace)).exists()) throw new Error("Parity profiles must start in a fresh owned workspace");
  const home = join(workspace, ".parity-home");
  await createHome(home);
  const traceRoot = join(home, ".parity");
  await mkdir(traceRoot, { recursive: true });
  const traces = Object.fromEntries(["effects", "probe", "host", "process", "provider", "network"].map(name => [name, join(traceRoot, `${name}.jsonl`)]));
  for (const file of Object.values(traces)) await writeFile(file, "");
  const launcher = await createLauncher(repoRoot, sourceRoot, workspace);
  const hostCommands = { pbcopy: "copy", pbpaste: "paste", "termux-clipboard-set": "copy", "termux-clipboard-get": "paste", open: "open", "xdg-open": "open", wslview: "open" };
  for (const [name, mode] of Object.entries(hostCommands)) {
    const executable = join(workspace, ".parity-bin", name);
    await writeFile(executable, `#!/bin/sh\nexec ${shellQuote(process.execPath)} ${shellQuote(join(repoRoot, "deploy/omp/parity/profile-host.ts"))} ${shellQuote(mode)} "$@"\n`);
    await chmod(executable, 0o700);
  }
  const parityRoot = join(repoRoot, "deploy/omp/parity");
  const environment = isolatedEnvironment(home, {
    PATH: [join(workspace, ".parity-bin"), process.env.PATH ?? "/usr/bin:/bin"].join(delimiter),
    OMP_PARITY_SOURCE_ROOT: sourceRoot,
    OMP_PARITY_INVENTORY: join(repoRoot, "target/omp-parity/inventory.json"),
    OMP_PARITY_EFFECTS: traces.effects!, OMP_PARITY_PROBE_TRACE: traces.probe!,
    OMP_PARITY_HOST_TRACE: traces.host!, OMP_PARITY_PROCESS_TRACE: traces.process!,
    OMP_PARITY_PROVIDER_TRACE: traces.provider!, OMP_PARITY_NETWORK_TRACE: traces.network!,
    OMP_PARITY_WORKSPACE: workspace,
    OMP_PARITY_ADAPTER: options.adapter,
    GIT_CONFIG_NOSYSTEM: "1",
  });
  await execute(["git", "init", "--initial-branch", "parity-main"], workspace, environment);
  await writeFile(join(workspace, ".git/info/exclude"), ".parity-home/\n.parity-bin/\n.parity-profile.json\nbunfig.toml\n");
  const scenario = options.scenario;
  const manifest: ProfileManifest = {
    version: 1, repoRoot, sourceRoot, workspace, home, environment,
    scenario: { id: scenario.scenario.id, kind: scenario.kind,
      files: scenario.kind === "builtin" ? scenario.scenario.files ?? {} : {},
      settings: scenario.kind === "builtin" ? scenario.scenario.settings ?? {} : {},
      ...(scenario.kind === "builtin" ? { requirements: scenario.requirements } : {}),
    }, launch: { memorySession: true },
  };
  await writeFile(profileManifestPath(workspace), `${JSON.stringify(manifest)}\n`);
  const seedCommand = [process.execPath, "--tsconfig-override", join(parityRoot, "tsconfig.json"), join(parityRoot, "seed-cli.ts"), workspace];
  await execute(seedCommand, workspace, environment);
  const seeded = await Bun.file(profileManifestPath(workspace)).json() as ProfileManifest;
  let services: RunningBuiltinServices | undefined;
  const provider = await startProvider(scenario.kind === "builtin" ? scenario.requirements.models.turns : [], traces.provider!, { networkTracePath: traces.network!, serviceTracePath: join(traceRoot, "http.jsonl") });
  try {
    if (scenario.kind === "builtin") {
      const builtin = await readBuiltinSeedManifest(home);
      services = await startBuiltinServices(builtin.services, { sourceRoot, workspace, home, sessionPath: builtin.sessionPath, load: sourceLoader(sourceRoot) });
      await writeFile(join(traceRoot, "service-settings.json"), JSON.stringify(services.settings));
      await execute([...seedCommand, "services"], workspace, environment);
    }
    const origins = new Set([new URL(provider.baseUrl).origin, ...(services?.environment.OMP_PARITY_ALLOWED_ORIGINS ?? "").split(",").filter(Boolean)]);
    Object.assign(environment, seeded.environment, services?.environment ?? {}, {
      OMP_PARITY_PROVIDER_URL: provider.baseUrl,
      OMP_PARITY_ALLOWED_ORIGINS: [...origins].join(","),
    });
    for (const [key, value] of Object.entries(environment)) if (typeof value !== "string") throw new Error(`Invalid profile environment ${key}`);
    seeded.environment = environment;
    await writeFile(profileManifestPath(workspace), `${JSON.stringify(seeded)}\n`);
    // Runtime reads local bunfig after a true /restart exec, which does not retain Bun CLI flags.
    if (Object.hasOwn(seeded.scenario.files, "bunfig.toml")) throw new Error("Scenario files cannot replace owned runtime preload configuration");
    await writeFile(join(workspace, "bunfig.toml"), `preload = [${JSON.stringify(join(parityRoot, "profile-start.ts"))}, ${JSON.stringify(join(parityRoot, "preload.ts"))}]\n`);
    await writeFile(join(traceRoot, "capture-request.json"), JSON.stringify({ channels: [], requests: [] }));
    assertOwned(workspace, home);
    return { home, workspace, environment, command: [launcher, "--cwd", workspace], effectsPath: traces.effects!, probeTracePath: traces.probe!, provider,
      async stop() {
        try { await stopProfileRuntime(repoRoot, sourceRoot, workspace, home, environment); }
        finally { try { await services?.dispose(); } finally { await provider.stop(); } }
      },
    };
  } catch (error) { try { await services?.dispose(); } finally { await provider.stop(); } throw error; }
}
