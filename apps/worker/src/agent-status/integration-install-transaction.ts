// Stages and commits one complete Roost agent-integration asset set.
// The installer supplies preflight snapshots; this owner revalidates directory
// identity and target ownership at commit, then rolls back partial mutations.

import { link, lstat, mkdir, mkdtemp, rename, rm, rmdir } from "node:fs/promises";
import { basename, join } from "node:path";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import type { SupportedHostPlatform } from "@roost/shared/platform";
import type { AgentIntegrationAssetId, AgentIntegrationRuntime } from "./integration-assets.ts";
import {
  assertIntegrationDirectorySnapshot,
  assertIntegrationTargetUnchanged,
  hasIntegrationOwnership,
  inspectIntegrationDirectory,
  inspectIntegrationTarget,
  integrationErrorCode,
  integrationLstatIfPresent,
  integrationPathComparisonKey,
  sameIntegrationFileSnapshot,
  sameIntegrationIdentity,
  type IntegrationDirectoryPlan,
  type IntegrationDirectorySnapshot,
  type IntegrationFileSnapshot,
} from "./integration-install-proof.ts";

export interface IntegrationAssetInstallPlan {
  id: AgentIntegrationAssetId;
  runtime: AgentIntegrationRuntime;
  target: string;
  content: string;
  ownershipMarker: string;
  existing: IntegrationFileSnapshot | null;
}

export interface IntegrationRetirementPlan {
  runtime: AgentIntegrationRuntime;
  target: string;
  ownershipMarker: string;
  existing: IntegrationFileSnapshot | null;
  remove: boolean;
}

export interface IntegrationInstallTestHooks {
  beforeFinalValidation?: () => void | Promise<void>;
  afterCommittedMutation?: (completedMutations: number) => void | Promise<void>;
}

interface PreparedDirectory {
  plan: IntegrationDirectoryPlan;
  snapshot: IntegrationDirectorySnapshot;
  created: boolean;
  stagePath: string;
  stageDevice: number;
  stageInode: number;
}

interface StagedAsset {
  plan: IntegrationAssetInstallPlan;
  path: string;
  device: number;
  inode: number;
}

interface AssetMutation {
  kind: "asset";
  directory: PreparedDirectory;
  target: string;
  staged: StagedAsset;
  backup: string | null;
  oldMoved: boolean;
  installed: boolean;
}

interface RetirementMutation {
  kind: "retirement";
  directory: PreparedDirectory;
  target: string;
  backup: string;
  oldMoved: boolean;
}

type InstallMutation = AssetMutation | RetirementMutation;

export async function commitIntegrationInstall(
  directoryPlans: Readonly<Record<AgentIntegrationRuntime, IntegrationDirectoryPlan>>,
  assets: readonly IntegrationAssetInstallPlan[],
  retirements: readonly IntegrationRetirementPlan[],
  platform: SupportedHostPlatform,
  hooks: IntegrationInstallTestHooks = {},
): Promise<void> {
  const prepared: Partial<Record<AgentIntegrationRuntime, PreparedDirectory>> = {};
  const staged: Partial<Record<AgentIntegrationAssetId, StagedAsset>> = {};
  const mutations: InstallMutation[] = [];
  try {
    for (const runtime of ["omp", "pi"] as const) {
      prepared[runtime] = await prepareDirectory(directoryPlans[runtime], platform);
    }
    const completePrepared = requiredPreparedDirectories(prepared);
    assertPreparedDirectoriesDistinct(completePrepared, platform);
    for (const asset of assets) {
      const directory = completePrepared[asset.runtime];
      await assertPreparedDirectoryStable(directory, platform);
      const path = join(directory.stagePath, `asset-${asset.id}`);
      await durableWriteFile(path, asset.content, {
        platform,
        mode: 0o600,
        privateDacl: true,
      });
      const metadata = await lstat(path);
      staged[asset.id] = {
        plan: asset,
        path,
        device: metadata.dev,
        inode: metadata.ino,
      };
    }

    await hooks.beforeFinalValidation?.();
    for (const directory of Object.values(completePrepared)) {
      await assertPreparedDirectoryStable(directory, platform);
    }
    assertPreparedDirectoriesDistinct(completePrepared, platform);
    for (const asset of assets) await assertIntegrationTargetUnchanged(asset);
    for (const retirement of retirements) {
      await assertIntegrationTargetUnchanged(retirement);
    }

    let completedMutations = 0;
    for (const asset of assets) {
      if (asset.existing?.content === asset.content) continue;
      const directory = completePrepared[asset.runtime];
      await assertPreparedDirectoryStable(directory, platform);
      await assertIntegrationTargetUnchanged(asset);
      const mutation = makeAssetMutation(
        requiredStagedAsset(staged, asset.id),
        directory,
      );
      mutations.push(mutation);
      await applyAssetMutation(mutation);
      completedMutations += 1;
      await hooks.afterCommittedMutation?.(completedMutations);
    }
    for (const retirement of retirements) {
      if (!retirement.remove) continue;
      const directory = completePrepared[retirement.runtime];
      await assertPreparedDirectoryStable(directory, platform);
      await assertIntegrationTargetUnchanged(retirement);
      const mutation = makeRetirementMutation(retirement, directory);
      mutations.push(mutation);
      await applyRetirementMutation(mutation, retirement);
      completedMutations += 1;
      await hooks.afterCommittedMutation?.(completedMutations);
    }
  } catch (error) {
    const rollbackComplete = await rollbackMutations(mutations, platform);
    if (rollbackComplete) await cleanupStages(prepared);
    await cleanupCreatedDirectories(prepared, platform);
    throw error;
  }
  await cleanupStages(prepared);
}

async function prepareDirectory(
  plan: IntegrationDirectoryPlan,
  platform: SupportedHostPlatform,
): Promise<PreparedDirectory> {
  let created = false;
  if (plan.initialSnapshot === null) {
    if (await integrationLstatIfPresent(plan.path)) {
      throw new Error(`agent integration loader changed before installation: ${plan.path}`);
    }
    await mkdir(plan.path, { recursive: true, mode: 0o700 });
    created = true;
  } else {
    await assertIntegrationDirectorySnapshot(plan, plan.initialSnapshot, platform);
  }
  const snapshot = await inspectIntegrationDirectory(plan.path);
  if (
    (plan.initialSnapshot === null && snapshot.entryIsSymlink) ||
    integrationPathComparisonKey(snapshot.canonicalPath, platform) !==
      integrationPathComparisonKey(plan.canonicalPath, platform)
  ) {
    throw new Error(`agent integration loader changed before installation: ${plan.path}`);
  }
  const stagePath = await mkdtemp(join(snapshot.canonicalPath, ".roost-integration-stage-"));
  const stageMetadata = await lstat(stagePath);
  return {
    plan,
    snapshot,
    created,
    stagePath,
    stageDevice: stageMetadata.dev,
    stageInode: stageMetadata.ino,
  };
}

function makeAssetMutation(
  staged: StagedAsset,
  directory: PreparedDirectory,
): AssetMutation {
  const plan = staged.plan;
  return {
    kind: "asset",
    directory,
    target: join(directory.snapshot.canonicalPath, basename(plan.target)),
    staged,
    backup: plan.existing === null
      ? null
      : join(directory.stagePath, `backup-${plan.id}`),
    oldMoved: false,
    installed: false,
  };
}

async function applyAssetMutation(mutation: AssetMutation): Promise<void> {
  const plan = mutation.staged.plan;
  if (mutation.backup !== null) {
    await rename(mutation.target, mutation.backup);
    mutation.oldMoved = true;
    const moved = await inspectIntegrationTarget(
      mutation.backup,
      "agent integration backup",
    );
    if (
      !sameIntegrationFileSnapshot(moved, plan.existing) ||
      !moved ||
      !hasIntegrationOwnership(moved.content, plan.ownershipMarker)
    ) {
      throw new Error(`agent integration target changed during installation: ${plan.target}`);
    }
  }
  try {
    await link(mutation.staged.path, mutation.target);
  } catch (error) {
    if (integrationErrorCode(error) === "EEXIST") {
      throw new Error(`agent integration target appeared during installation: ${plan.target}`);
    }
    throw error;
  }
  mutation.installed = true;
  const installed = await lstat(mutation.target);
  if (!sameIntegrationIdentity(installed, mutation.staged)) {
    throw new Error(`agent integration target changed during installation: ${plan.target}`);
  }
}

function makeRetirementMutation(
  plan: IntegrationRetirementPlan,
  directory: PreparedDirectory,
): RetirementMutation {
  return {
    kind: "retirement",
    directory,
    target: join(directory.snapshot.canonicalPath, basename(plan.target)),
    backup: join(directory.stagePath, `retired-${basename(plan.target)}`),
    oldMoved: false,
  };
}

async function applyRetirementMutation(
  mutation: RetirementMutation,
  plan: IntegrationRetirementPlan,
): Promise<void> {
  await rename(mutation.target, mutation.backup);
  mutation.oldMoved = true;
  const moved = await inspectIntegrationTarget(
    mutation.backup,
    "retired agent integration backup",
  );
  if (
    !sameIntegrationFileSnapshot(moved, plan.existing) ||
    !moved ||
    !hasIntegrationOwnership(moved.content, plan.ownershipMarker)
  ) {
    throw new Error(`retired agent integration changed during installation: ${plan.target}`);
  }
}

async function rollbackMutations(
  mutations: readonly InstallMutation[],
  platform: SupportedHostPlatform,
): Promise<boolean> {
  let complete = true;
  for (const mutation of [...mutations].reverse()) {
    try {
      await assertPreparedDirectoryStable(mutation.directory, platform);
      if (mutation.kind === "asset" && mutation.installed) {
        const installed = await integrationLstatIfPresent(mutation.target);
        if (installed && sameIntegrationIdentity(installed, mutation.staged)) {
          await durableRemove(mutation.target, { platform });
        } else if (installed) {
          complete = false;
        }
      }
      if (mutation.oldMoved) {
        const backup = mutation.backup;
        if (!backup) {
          complete = false;
          continue;
        }
        const target = await integrationLstatIfPresent(mutation.target);
        if (target === null) {
          await link(backup, mutation.target);
        } else {
          const backupMetadata = await lstat(backup);
          if (!sameIntegrationIdentity(target, {
            device: backupMetadata.dev,
            inode: backupMetadata.ino,
          })) complete = false;
        }
      }
    } catch {
      complete = false;
    }
  }
  return complete;
}

async function assertPreparedDirectoryStable(
  prepared: PreparedDirectory,
  platform: SupportedHostPlatform,
): Promise<void> {
  await assertIntegrationDirectorySnapshot(prepared.plan, prepared.snapshot, platform);
}

function assertPreparedDirectoriesDistinct(
  prepared: Readonly<Record<AgentIntegrationRuntime, PreparedDirectory>>,
  platform: SupportedHostPlatform,
): void {
  const omp = prepared.omp.snapshot.canonicalPath;
  const pi = prepared.pi.snapshot.canonicalPath;
  if (
    integrationPathComparisonKey(omp, platform) ===
      integrationPathComparisonKey(pi, platform)
  ) {
    throw new Error(
      "refusing colliding OMP and Pi integration directories; configure distinct roots",
    );
  }
}

async function cleanupStages(
  prepared: Readonly<Partial<Record<AgentIntegrationRuntime, PreparedDirectory>>>,
): Promise<void> {
  for (const directory of Object.values(prepared)) {
    if (!directory) continue;
    try {
      const parent = await lstat(directory.snapshot.canonicalPath);
      const stage = await lstat(directory.stagePath);
      if (
        sameIntegrationIdentity(parent, {
          device: directory.snapshot.directoryDevice,
          inode: directory.snapshot.directoryInode,
        }) &&
        stage.isDirectory() &&
        !stage.isSymbolicLink() &&
        stage.dev === directory.stageDevice &&
        stage.ino === directory.stageInode
      ) {
        await rm(directory.stagePath, { recursive: true, force: true });
      }
    } catch {
      // A changed canonical directory is never followed during cleanup.
    }
  }
}

async function cleanupCreatedDirectories(
  prepared: Readonly<Partial<Record<AgentIntegrationRuntime, PreparedDirectory>>>,
  platform: SupportedHostPlatform,
): Promise<void> {
  for (const directory of Object.values(prepared).reverse()) {
    if (!directory?.created) continue;
    try {
      await assertPreparedDirectoryStable(directory, platform);
      await rmdir(directory.snapshot.canonicalPath);
    } catch {
      // Non-empty or raced directories belong to their current owner.
    }
  }
}

function requiredPreparedDirectories(
  prepared: Readonly<Partial<Record<AgentIntegrationRuntime, PreparedDirectory>>>,
): Record<AgentIntegrationRuntime, PreparedDirectory> {
  if (!prepared.omp || !prepared.pi) {
    throw new Error("agent integration staging directories are incomplete");
  }
  return { omp: prepared.omp, pi: prepared.pi };
}

function requiredStagedAsset(
  staged: Readonly<Partial<Record<AgentIntegrationAssetId, StagedAsset>>>,
  id: AgentIntegrationAssetId,
): StagedAsset {
  const asset = staged[id];
  if (!asset) throw new Error(`missing staged agent integration asset: ${id}`);
  return asset;
}
