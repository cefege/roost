import { join } from "node:path";
import type { CaptureState } from "./builtin-capture-state";
import { assignPath, ownedPath, readOwnedText, requestedPointers } from "./builtin-capture-state";
import { captureSecurityWorkflow } from "./builtin-capture-security-workflows";
import { probe } from "./probe";

async function marketplaceManager(state: CaptureState) {
  const marketplace = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/extensibility/plugins/marketplace/index")>("packages/coding-agent/src/extensibility/plugins/marketplace/index.ts");
  const helpers = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/discovery/helpers")>("packages/coding-agent/src/discovery/helpers.ts");
  const projectPath = await helpers.resolveOrDefaultProjectRegistryPath(state.context.workspace);
  const options = {
    marketplacesRegistryPath: await ownedPath(state, marketplace.getMarketplacesRegistryPath()),
    installedRegistryPath: await ownedPath(state, marketplace.getInstalledPluginsRegistryPath()),
    projectInstalledRegistryPath: projectPath ? await ownedPath(state, projectPath) : projectPath,
    marketplacesCacheDir: await ownedPath(state, marketplace.getMarketplacesCacheDir()),
    pluginsCacheDir: await ownedPath(state, marketplace.getPluginsCacheDir()),
    clearPluginRootsCache: helpers.clearPluginRootsAndCaches,
  };
  return new marketplace.MarketplaceManager(options);
}

export async function captureMarketplaces(state: CaptureState): Promise<unknown> {
  return (await marketplaceManager(state)).listMarketplaces();
}

export async function capturePlugins(state: CaptureState): Promise<unknown> {
  const plugins = await (await marketplaceManager(state)).listInstalledPlugins();
  const result: Record<string, unknown> = {};
  for (const plugin of plugins) {
    if (Object.hasOwn(result, plugin.id)) continue;
    result[plugin.id] = { ...plugin, installed: plugin.entries.length > 0, enabled: plugin.entries.some(entry => entry.enabled !== false) };
  }
  for (const parts of requestedPointers(state, "builtin.plugins")) {
    const id = parts[0];
    if (!Object.hasOwn(result, id)) result[id] = { installed: false };
  }
  return result;
}

export async function captureSkills(state: CaptureState): Promise<unknown> {
  const installer = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/skillshare/installer")>("packages/coding-agent/src/skillshare/installer.ts");
  const manifest = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/skillshare/manifest")>("packages/coding-agent/src/skillshare/manifest.ts");
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  await ownedPath(state, manifest.getGlobalSkillsInstallPaths().lock);
  await ownedPath(state, (await manifest.getProjectSkillsInstallPaths(state.context.workspace)).lock);
  const result: Record<string, unknown> = {};
  for (const skill of await installer.listInstalledSkills(state.context.workspace)) {
    if (Object.hasOwn(result, skill.id)) continue;
    const observation: Record<string, unknown> = { ...skill };
    if (skill.version && skill.stored) {
      const parsed = manifest.parseSkillId(skill.id);
      if (!parsed) throw new Error(`Invalid actual installed skill id: ${skill.id}`);
      const file = join(manifest.getSkillStorePath(parsed.scope, parsed.name, skill.version), "SKILL.md");
      const content = await readOwnedText(state, file);
      utilities.parseFrontmatter(content, { source: file, repair: false, level: "fatal" });
      const delimiter = /^---\r?\n[\s\S]*?\r?\n---(?:\r?\n|$)/.exec(content);
      if (!delimiter) throw new Error(`Installed registry skill lacks frontmatter: ${file}`);
      observation.content = content.slice(delimiter[0].length).replace(/^\r?\n/, "");
    }
    result[skill.id] = observation;
  }
  return result;
}

export async function captureSecurity(state: CaptureState): Promise<unknown> {
  const module = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/security/store")>("packages/coding-agent/src/security/store.ts");
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  await ownedPath(state, utilities.getSecurityProjectDir("parity-capture-ownership"));
  const store = await module.SecurityStore.openForCwd(await ownedPath(state, probe.requireSession().sessionManager.getCwd()));
  await ownedPath(state, store.projectDirectory);
  const scans = await store.listScans();
  const bundles = await Promise.all(scans.map(scan => store.getBundle(scan.id)));
  const result: Record<string, unknown> = { scans, bundles };
  const findings = bundles.flatMap(bundle => bundle?.findings ?? []);
  for (const finding of findings) result[finding.id] = finding;
  const imported = bundles.filter(bundle => bundle !== null && bundle.scan.producer.kind === "sarif-import");
  const importedFindings = imported.flatMap(bundle => bundle?.findings ?? []);
  result.importedFindingCount = importedFindings.length;
  if (importedFindings.length > 0) result.importedTitle = importedFindings[0].title;
  if (requestedPointers(state, "builtin.security").some(parts => parts[0] === "workflow")) result.workflow = await captureSecurityWorkflow(state, store);
  return result;
}
export async function captureSsh(state: CaptureState): Promise<unknown> {
  const writer = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/ssh/config-writer")>("packages/coding-agent/src/ssh/config-writer.ts");
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  const result: Record<string, unknown> = {};
  for (const scope of ["project", "user"] as const) {
    result[scope] = await writer.readSSHConfigFile(await ownedPath(state, utilities.getSSHConfigPath(scope, state.context.workspace)));
  }
  for (const parts of requestedPointers(state, "builtin.ssh")) {
    if (parts.at(-1) !== "exists") continue;
    let parent: unknown = result;
    for (const part of parts.slice(0, -2)) parent = typeof parent === "object" && parent !== null ? (parent as Record<string, unknown>)[part] : undefined;
    const present = typeof parent === "object" && parent !== null && Object.hasOwn(parent, parts[parts.length - 2]);
    assignPath(result, parts, present);
  }
  return result;
}

export async function captureSmithery(state: CaptureState): Promise<unknown> {
  const utilities = await state.context.load<typeof import("@omp-source/packages/utils/src/index")>("packages/utils/src/index.ts");
  await ownedPath(state, join(utilities.getAgentDir(), "smithery.json"));
  if (process.env.SMITHERY_API_KEY) throw new Error("Smithery capture forbids inherited API keys outside the owned store");
  const auth = await state.context.load<typeof import("@omp-source/packages/coding-agent/src/mcp/smithery-auth")>("packages/coding-agent/src/mcp/smithery-auth.ts");
  return { hasApiKey: (await auth.getSmitheryApiKey()) !== undefined };
}
