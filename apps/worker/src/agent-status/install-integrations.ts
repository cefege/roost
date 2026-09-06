// Plans installation of the complete typed agent-integration asset set.
// It rejects path aliases and unowned targets before handing an immutable
// preflight plan to the staged, rollback-capable filesystem transaction.

import { homedir } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import {
  supportedHostPlatform,
  type SupportedHostPlatform,
} from "@roost/shared/platform";
import { AGENT_INTEGRATION_ASSETS } from "./integration-assets.generated.ts";
import {
  AGENT_INTEGRATION_ASSET_SPECS,
  RETIRED_AGENT_INTEGRATION_SPECS,
  type AgentIntegrationAssetId,
  type AgentIntegrationAssetSpec,
  type AgentIntegrationRuntime,
  type EmbeddedAgentIntegrationAsset,
} from "./integration-assets.ts";
import {
  hasIntegrationOwnership,
  inspectIntegrationTarget,
  integrationPathComparisonKey,
  preflightIntegrationDirectory,
  type IntegrationDirectoryPlan,
} from "./integration-install-proof.ts";
import {
  commitIntegrationInstall,
  type IntegrationAssetInstallPlan,
  type IntegrationInstallTestHooks,
  type IntegrationRetirementPlan,
} from "./integration-install-transaction.ts";
import { composeStandaloneIntegration } from "./standalone-integration.ts";

export interface MaterializedAgentIntegrationAsset {
  spec: AgentIntegrationAssetSpec;
  content: string;
}

export interface InstalledAgentIntegration {
  id: AgentIntegrationAssetId;
  path: string;
}

export interface AgentIntegrationInstallerTestOptions
  extends IntegrationInstallTestHooks {
  platform?: SupportedHostPlatform;
}

export function resolvePiExtensionDir(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): string {
  const configured = env.PI_CODING_AGENT_DIR?.trim();
  const agentDir = configured
    ? expandHome(configured, home)
    : join(home, ".pi", "agent");
  return join(agentDir, "extensions");
}

export function resolveOmpExtensionDir(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): string {
  const sharedAgentDir = env.PI_CODING_AGENT_DIR?.trim();
  if (sharedAgentDir) {
    return join(expandHome(sharedAgentDir, home), "extensions");
  }
  const configured = expandHome(env.PI_CONFIG_DIR?.trim() || ".omp", home);
  const configDir = isAbsolute(configured)
    ? configured
    : join(home, configured);
  return join(configDir, "agent", "extensions");
}

export async function _loadAgentIntegrationAssets(): Promise<
  readonly MaterializedAgentIntegrationAsset[]
> {
  if (AGENT_INTEGRATION_ASSETS.length > 0) {
    return materializeEmbeddedAssets(AGENT_INTEGRATION_ASSETS);
  }
  const transport = await Bun.file(
    new URL("./report-transport.ts", import.meta.url),
  ).text();
  return Promise.all(AGENT_INTEGRATION_ASSET_SPECS.map(async (spec) => {
    const source = await Bun.file(
      new URL(`./${spec.sourcePath}`, import.meta.url),
    ).text();
    const content = composeStandaloneIntegration(source, transport);
    assertOwnedContent(spec, content);
    return { spec, content };
  }));
}

export async function installAgentIntegrations(
  env: NodeJS.ProcessEnv = process.env,
  home = homedir(),
): Promise<readonly InstalledAgentIntegration[]> {
  return installAgentIntegrationsWithOptions(env, home, {});
}

export async function _installAgentIntegrationsForTest(
  env: NodeJS.ProcessEnv,
  home: string,
  options: AgentIntegrationInstallerTestOptions,
): Promise<readonly InstalledAgentIntegration[]> {
  return installAgentIntegrationsWithOptions(env, home, options);
}

async function installAgentIntegrationsWithOptions(
  env: NodeJS.ProcessEnv,
  home: string,
  options: AgentIntegrationInstallerTestOptions,
): Promise<readonly InstalledAgentIntegration[]> {
  const assets = await _loadAgentIntegrationAssets();
  const platform = options.platform ?? supportedHostPlatform();
  const directoryPaths: Record<AgentIntegrationRuntime, string> = {
    omp: resolveOmpExtensionDir(env, home),
    pi: resolvePiExtensionDir(env, home),
  };
  const directoryPlans: Record<
    AgentIntegrationRuntime,
    IntegrationDirectoryPlan
  > = {
    omp: await preflightIntegrationDirectory(directoryPaths.omp),
    pi: await preflightIntegrationDirectory(directoryPaths.pi),
  };
  if (
    integrationPathComparisonKey(directoryPlans.omp.canonicalPath, platform) ===
      integrationPathComparisonKey(directoryPlans.pi.canonicalPath, platform)
  ) {
    throw new Error(
      "refusing colliding OMP and Pi integration directories; configure distinct roots",
    );
  }

  const plannedAssets: IntegrationAssetInstallPlan[] = await Promise.all(
    assets.map(async ({ spec, content }) => {
      const target = join(directoryPaths[spec.runtime], spec.installFilename);
      const existing = await inspectIntegrationTarget(
        target,
        "agent integration target",
      );
      if (
        existing && existing.content !== content &&
        !hasIntegrationOwnership(existing.content, spec.ownershipMarker)
      ) {
        throw new Error(`refusing to overwrite non-Roost extension: ${target}`);
      }
      return {
        id: spec.id,
        runtime: spec.runtime,
        target,
        content,
        ownershipMarker: spec.ownershipMarker,
        existing,
      };
    }),
  );
  const plannedRetirements: IntegrationRetirementPlan[] = await Promise.all(
    RETIRED_AGENT_INTEGRATION_SPECS.map(async (spec) => {
      const target = join(directoryPaths[spec.runtime], spec.installFilename);
      const existing = await inspectIntegrationTarget(
        target,
        "retired agent integration",
      );
      return {
        runtime: spec.runtime,
        target,
        ownershipMarker: spec.ownershipMarker,
        existing,
        remove: !!existing && hasIntegrationOwnership(
          existing.content,
          spec.ownershipMarker,
        ),
      };
    }),
  );
  assertUniqueTargets(
    [...plannedAssets, ...plannedRetirements].map(({ runtime, target }) => ({
      canonicalTarget: join(
        directoryPlans[runtime].canonicalPath,
        basename(target),
      ),
    })),
    platform,
  );

  await commitIntegrationInstall(
    directoryPlans,
    plannedAssets,
    plannedRetirements,
    platform,
    options,
  );
  return plannedAssets.map(({ id, target }) => ({ id, path: target }));
}

function expandHome(value: string, home: string): string {
  if (value === "~") return home;
  if (value.startsWith("~/") || value.startsWith("~\\")) {
    return join(home, value.slice(2));
  }
  return value;
}

function materializeEmbeddedAssets(
  embedded: readonly EmbeddedAgentIntegrationAsset[],
): readonly MaterializedAgentIntegrationAsset[] {
  if (embedded.length !== AGENT_INTEGRATION_ASSET_SPECS.length) {
    throw new Error("generated agent integration asset set is incomplete");
  }
  const contentById = new Map<AgentIntegrationAssetId, string>();
  for (const asset of embedded) {
    if (contentById.has(asset.id) || asset.content.length === 0) {
      throw new Error("generated agent integration asset set is invalid");
    }
    contentById.set(asset.id, asset.content);
  }
  return AGENT_INTEGRATION_ASSET_SPECS.map((spec) => {
    const content = contentById.get(spec.id);
    if (content === undefined) {
      throw new Error("generated agent integration asset set is incomplete");
    }
    assertOwnedContent(spec, content);
    return { spec, content };
  });
}

function assertOwnedContent(
  spec: AgentIntegrationAssetSpec,
  content: string,
): void {
  if (!hasIntegrationOwnership(content, spec.ownershipMarker)) {
    throw new Error(`agent integration asset ${spec.id} lost its ownership marker`);
  }
}

function assertUniqueTargets(
  targets: readonly { canonicalTarget: string }[],
  platform: SupportedHostPlatform,
): void {
  const seen = new Set<string>();
  for (const target of targets) {
    const comparable = integrationPathComparisonKey(
      target.canonicalTarget,
      platform,
    );
    if (seen.has(comparable)) {
      throw new Error("refusing colliding agent integration target paths");
    }
    seen.add(comparable);
  }
}
