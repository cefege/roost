import { join, resolve } from "node:path";
import { sourceLoader } from "./source-load";
import { readProfile } from "./profile-manifest";
import { readBuiltinSeedManifest, seedBuiltin, writeOwnedBuiltinFile } from "./builtin-seed";
import { profileModelSelection, seedProfileAuth, writeProfileModels } from "./profile-models";

const workspace = resolve(process.argv[2] ?? "");
const phase = process.argv[3] ?? "seed";
const manifest = await readProfile(workspace);
if (resolve(process.env.HOME ?? "") !== manifest.home || process.env.PI_CODING_AGENT_DIR !== join(manifest.home, ".omp/agent")) throw new Error("Seeding requires the owned environment before importing any production module");
process.chdir(workspace);
const load = sourceLoader(manifest.sourceRoot);
if (phase === "models") {
  await writeProfileModels(manifest, load);
  process.exit(0);
}
if (phase !== "seed" && phase !== "services") throw new Error(`Unknown parity seeding phase ${phase}`);
if (phase === "seed") await seedProfileAuth(manifest, load);
await load<typeof import("@omp-source/packages/coding-agent/src/config/all-settings")>("packages/coding-agent/src/config/all-settings.ts");
const { Settings } = await load<typeof import("@omp-source/packages/coding-agent/src/config/settings")>("packages/coding-agent/src/config/settings.ts");
const { lookup } = await load<typeof import("@omp-source/packages/coding-agent/src/config/registry")>("packages/coding-agent/src/config/registry.ts");
const settings = await Settings.loadIsolated({ cwd: workspace, agentDir: process.env.PI_CODING_AGENT_DIR });
const base: Record<string, unknown> = {
  autoResume: false, "retry.enabled": false, "retry.modelFallback": false,
  "compaction.enabled": false, "startup.showSplash": false, "startup.setupWizard": false,
  "startup.checkUpdate": false, "startup.changelogMode": "hidden", "marketplace.autoUpdate": "off",
  "telemetry.otlpExportEnabled": false,
};
let values: Record<string, unknown>;
if (phase === "seed") {
  for (const [name, content] of Object.entries(manifest.scenario.files)) await writeOwnedBuiltinFile(workspace, name, content);
  const model = profileModelSelection(manifest);
  settings.setModelRole("default", `${model.provider}/${model.defaultId}`);
  values = { ...base, ...manifest.scenario.settings };
} else if (phase === "services") {
  values = await Bun.file(join(manifest.home, ".parity/service-settings.json")).json() as Record<string, unknown>;
} else throw new Error(`Unknown parity seeding phase ${phase}`);
for (const [id, value] of Object.entries(values)) {
  const setting = lookup(id);
  if (!setting) throw new Error(`Unregistered real setting in parity profile: ${id}`);
  setting.set(settings, value);
}
await settings.flush();
if (phase === "seed" && manifest.scenario.kind === "builtin") {
  const requirement = manifest.scenario.requirements;
  if (!requirement) throw new Error("Builtin profile lacks explicit source-derived requirements");
  await seedBuiltin(requirement, { sourceRoot: manifest.sourceRoot, workspace, home: manifest.home, load });
  const seeded = await readBuiltinSeedManifest(manifest.home);
  manifest.launch = { sessionPath: seeded.sessionPath, memorySession: seeded.launch.memorySession };
  Object.assign(manifest.environment, seeded.environment);
  for (const key of seeded.unsetEnvironment) delete manifest.environment[key];
  if (seeded.launch.omitTools.length > 0) {
    const { BUILTIN_TOOLS } = await load<typeof import("@omp-source/packages/coding-agent/src/tools")>("packages/coding-agent/src/tools/index.ts");
    manifest.launch.tools = Object.keys(BUILTIN_TOOLS).filter(name => !seeded.launch.omitTools.includes(name));
  }
  await Bun.write(join(workspace, ".parity-profile.json"), `${JSON.stringify(manifest)}\n`);
}
process.exit(0);
