// Plans installation of the complete typed agent-integration asset set.
// It rejects path aliases and unowned targets before handing an immutable
// preflight plan to the staged, rollback-capable filesystem transaction.

import { homedir } from "node:os";
import { basename, isAbsolute, join } from "node:path";
import { log } from "@roost/shared/log";
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

export interface FailedAgentIntegration {
  runtime: AgentIntegrationRuntime;
  path: string;
  reason: string;
}

export interface AgentIntegrationInstallReport {
  installed: readonly InstalledAgentIntegration[];
  failed: readonly FailedAgentIntegration[];
}

type IntegrationPlanOutcome<TPlan> =
  | { kind: "planned"; plan: TPlan }
  | { kind: "failed"; failure: FailedAgentIntegration };

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
): Promise<AgentIntegrationInstallReport> {
  return installAgentIntegrationsWithOptions(env, home, {});
}

export async function _installAgentIntegrationsForTest(
  env: NodeJS.ProcessEnv,
  home: string,
  options: AgentIntegrationInstallerTestOptions,
): Promise<AgentIntegrationInstallReport> {
  return installAgentIntegrationsWithOptions(env, home, options);
}

async function installAgentIntegrationsWithOptions(
  env: NodeJS.ProcessEnv,
  home: string,
  options: AgentIntegrationInstallerTestOptions,
): Promise<AgentIntegrationInstallReport> {
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

  const assetCandidates = assets.map(({ spec, content }) => ({
    spec,
    content,
    target: join(directoryPaths[spec.runtime], spec.installFilename),
  }));
  const retirementCandidates = RETIRED_AGENT_INTEGRATION_SPECS.map((spec) => ({
    spec,
    target: join(directoryPaths[spec.runtime], spec.installFilename),
  }));
  // Collision is a property of the catalog and the two directories, so it is
  // proven over every candidate: a target dropped by its own planning failure
  // must not relax the check for the targets that still install.
  assertUniqueTargets(
    [...assetCandidates, ...retirementCandidates].map(({ spec, target }) => ({
      canonicalTarget: join(
        directoryPlans[spec.runtime].canonicalPath,
        basename(target),
      ),
    })),
    platform,
  );

  const assetOutcomes = await Promise.all(
    assetCandidates.map(({ spec, content, target }) =>
      capturePlanFailure(spec.runtime, target, () =>
        planAssetInstall(spec, content, target)
      )
    ),
  );
  const retirementOutcomes = await Promise.all(
    retirementCandidates.map(({ spec, target }) =>
      capturePlanFailure(spec.runtime, target, () => planRetirement(spec, target))
    ),
  );
  const plannedAssets: IntegrationAssetInstallPlan[] = [];
  const plannedRetirements: IntegrationRetirementPlan[] = [];
  const failed: FailedAgentIntegration[] = [];
  for (const outcome of assetOutcomes) {
    if (outcome.kind === "planned") plannedAssets.push(outcome.plan);
    else failed.push(outcome.failure);
  }
  for (const outcome of retirementOutcomes) {
    if (outcome.kind === "planned") plannedRetirements.push(outcome.plan);
    else failed.push(outcome.failure);
  }
  for (const failure of failed) {
    log.warn("agent-status", "integration_install_failed", {
      runtime: failure.runtime,
      path: failure.path,
      error: failure.reason,
    });
  }

  await commitIntegrationInstall(
    directoryPlans,
    plannedAssets,
    plannedRetirements,
    platform,
    options,
  );
  return {
    installed: plannedAssets.map(({ id, target }) => ({ id, path: target })),
    failed,
  };
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

/** One refusal is one asset's problem: a target Roost may not write must not
 *  cancel the targets it may, so every planning failure becomes an outcome the
 *  pass reports instead of a throw that aborts the whole install. */
async function capturePlanFailure<TPlan>(
  runtime: AgentIntegrationRuntime,
  target: string,
  planTarget: () => Promise<TPlan>,
): Promise<IntegrationPlanOutcome<TPlan>> {
  try {
    return { kind: "planned", plan: await planTarget() };
  } catch (error) {
    return {
      kind: "failed",
      failure: { runtime, path: target, reason: String(error) },
    };
  }
}

async function planAssetInstall(
  spec: AgentIntegrationAssetSpec,
  content: string,
  target: string,
): Promise<IntegrationAssetInstallPlan> {
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
}

async function planRetirement(
  spec: Pick<
    AgentIntegrationAssetSpec,
    "runtime" | "installFilename" | "ownershipMarker"
  >,
  target: string,
): Promise<IntegrationRetirementPlan> {
  const existing = await inspectIntegrationTarget(
    target,
    "retired agent integration",
  );
  return {
    runtime: spec.runtime,
    target,
    ownershipMarker: spec.ownershipMarker,
    existing,
    remove: !!existing &&
      hasIntegrationOwnership(existing.content, spec.ownershipMarker),
  };
}
