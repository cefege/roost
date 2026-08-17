import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readFile, readdir, rm } from "node:fs/promises";
import { basename, dirname, isAbsolute, join, relative, resolve, win32 } from "node:path";
import {
  durableReplace,
  flushDurablePath,
} from "@roost/shared/durability";
import { acquireMachineTransaction } from "@roost/shared/machine-transaction";
import { roostServiceDir, roostVersionsDir } from "@roost/shared/paths";
import type { ServiceHealthRole, ServiceHealthStatusFor } from "@roost/shared/service-health";
import {
  WINDOWS_SERVICE_NAMES,
  quoteWindowsArg,
  retargetWindowsUpdaterDefinition,
} from "./service-ctl.ts";
import type {
  RoostServiceRole,
  WindowsServiceDefinition,
  WindowsServiceManager,
  WindowsServiceSnapshot,
  WindowsServiceSnapshotSet,
} from "./service-ctl.ts";
import {
  appendWindowsUpdateProgress,
  assertWindowsUpdateJournal,
  parseWindowsCurrentManifest,
  parseWindowsReleaseManifest,
  sha256Hex,
  replaceWindowsUpdaterArtifact,
  windowsCurrentManifestPath,
} from "./windows-update-journal.ts";
import type {
  WindowsCurrentManifestV1,
  WindowsReleaseFile,
  WindowsUpdateForwardPhase,
  WindowsUpdateJournalStore,
  WindowsUpdateJournalV1,
  WindowsUpdateJournalV2,
  WindowsUpdateRollbackPhase,
  WindowsStableArtifactPlan,
  WindowsUpdaterPersistenceProfile,
} from "./windows-update-journal.ts";

const ACTIVE_ROLES = ["coordinator", "worker"] as const satisfies readonly RoostServiceRole[];
const STOP_ORDER = ["worker", "coordinator", "keeper"] as const satisfies readonly RoostServiceRole[];
const START_ORDER = ["keeper", "coordinator", "worker"] as const satisfies readonly RoostServiceRole[];
const SERVICE_ROLES = ["keeper", "worker", "coordinator", "updater"] as const satisfies readonly RoostServiceRole[];
const FORWARD_ORDER: readonly WindowsUpdateForwardPhase[] = [
  "prepared",
  "broker-started",
  "assets-staged",
  "stable-artifacts-snapshotted",
  "services-stopped",
  "stable-artifacts-promoted",
  "updater-config-switched",
  "current-manifest-switched",
  "services-restored",
  "health-proven",
  "committed",
  "cleanup-complete",
];
const ROLLBACK_ORDER = [
  "rollback-started",
  "rollback-services-stopped",
  "rollback-stable-artifacts-restored",
  "rollback-configs-restored",
  "rollback-current-manifest-restored",
  "rollback-services-restored",
  "rolled-back",
] as const satisfies readonly WindowsUpdateRollbackPhase[];

export interface WindowsUpdateServiceManager {
  query(role: RoostServiceRole, options?: { includeSecurity?: boolean }): Promise<WindowsServiceSnapshot>;
  snapshot?(options?: { includeSecurity?: boolean }): Promise<WindowsServiceSnapshotSet>;
  start(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  stop(role: RoostServiceRole, options?: { timeoutMs?: number }): Promise<WindowsServiceSnapshot>;
  install(
    definition: WindowsServiceDefinition,
  ): Promise<WindowsServiceSnapshot>;
  restore?(
    snapshot: WindowsServiceSnapshotSet,
    options?: { restoreLifecycleRoles?: readonly RoostServiceRole[]; allowKeeperStop?: boolean },
  ): Promise<WindowsServiceSnapshotSet>;
}

export interface WindowsUpdateNative {
  assertUpdaterServiceContext(): Promise<void>;
  verifyCmsDetached(manifestPath: string, signaturePath: string, publisherSha256: string): Promise<void>;
  verifyAuthenticode(path: string, publisherSha256: string): Promise<void>;
  extractZip(packagePath: string, destination: string, files: readonly WindowsReleaseFile[]): Promise<void>;
  probeExclusiveOpen(path: string): Promise<boolean>;
  protectArtifacts(path: string): Promise<void>;
  readArtifact?(
    path: string,
    profile: WindowsUpdaterPersistenceProfile,
    maxBytes: number,
  ): Promise<Uint8Array>;
  replaceArtifact?(
    path: string,
    profile: WindowsUpdaterPersistenceProfile,
    contents: string | Uint8Array,
  ): Promise<void>;
  copyArtifact?(
    sourcePath: string,
    destinationPath: string,
    sourceProfile: WindowsUpdaterPersistenceProfile,
    destinationProfile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{
    sourceProfile: WindowsUpdaterPersistenceProfile;
    destinationProfile: WindowsUpdaterPersistenceProfile;
    sha256: string;
    size: number;
    sddl: string;
  }>;
  inspectArtifact?(
    path: string,
    profile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{
    profile: WindowsUpdaterPersistenceProfile;
    sha256: string;
    size: number;
    sddl: string;
  }>;
}

export type WindowsUpdateHealthDescriptor<
  R extends ServiceHealthRole = ServiceHealthRole,
> = ServiceHealthStatusFor<R>;

export interface ServiceHealthProver {
  read<R extends ServiceHealthRole>(role: R): Promise<WindowsUpdateHealthDescriptor<R>>;
  prove(
    role: ServiceHealthRole,
    journal: Readonly<WindowsUpdateJournalV1 | WindowsUpdateJournalV2>,
    mode: "forward" | "proof" | "rollback",
  ): Promise<void>;
}

export interface WindowsUpdateBrokerDeps {
  store: WindowsUpdateJournalStore;
  services: WindowsUpdateServiceManager;
  native: WindowsUpdateNative;
  health: ServiceHealthProver;
  serviceDir?: string;
  versionsDir?: string;
  currentManifestPath?: string;
  acquireTransaction?: typeof acquireMachineTransaction;
  writeCurrentManifest?: (path: string, contents: string | null) => Promise<void>;
  now?: () => Date;
  fetch?: typeof fetch;
  requestDir?: string;
  platform?: NodeJS.Platform;
}

interface BrokerRoots {
  serviceDir: string;
  versionsDir: string;
  currentManifestPath: string;
}

export async function runWindowsUpdateBroker(
  deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2 | null> {
  await deps.native.assertUpdaterServiceContext();
  const transaction = await (deps.acquireTransaction ?? acquireMachineTransaction)("update", deps.store.path);
  try {
    let loaded = await deps.store.load();
    const terminal = loaded && (loaded.state === "succeeded" || loaded.state === "rolled-back");
    if ((deps.platform ?? process.platform) === "win32" && (!loaded || terminal)) {
      const roots = brokerRoots(deps);
      const { admitPendingWindowsUpdateRequestWithinTransaction } = await import(
        "./windows-update-control.ts"
      );
      const admitted = await admitPendingWindowsUpdateRequestWithinTransaction({
        store: deps.store,
        services: deps.services as WindowsServiceManager,
        native: deps.native,
        fetch: deps.fetch ?? fetch,
        serviceDir: roots.serviceDir,
        versionsDir: roots.versionsDir,
        currentManifestPath: roots.currentManifestPath,
        requestDir: deps.requestDir ?? join(roots.serviceDir, "requests"),
        probeHealth: async <R extends ServiceHealthRole>(role: R) => await deps.health.read(role),
        platform: "win32",
      });
      if (admitted) loaded = admitted;
    }
    // An active durable journal always wins over a pending admission.
    if (!loaded) return null;
    if (loaded.schemaVersion === 1 && (loaded.state === "succeeded" || loaded.state === "rolled-back")) {
      return null;
    }
    if (loaded.schemaVersion === 1) {
      throw new Error(
        "legacy Windows update topology requires signed elevated installer migration before mutation",
      );
    }
    let journal = loaded;
    if (journal.state === "succeeded" || journal.state === "rolled-back") {
      await deps.store.save(journal);
      return journal;
    }
    await assertJournalPathBindings(journal, deps);
    if (
      journal.state === "forward"
      && phaseAtLeast(journal.phase, "stable-artifacts-snapshotted")
      && !phaseAtLeast(journal.phase, "committed")
    ) {
      const forwardPhase = isForward(journal.phase) ? journal.phase : "stable-artifacts-snapshotted";
      const failure = {
        forwardPhase,
        error: "updater process restarted after the authoritative mutation boundary",
        at: now(deps).toISOString(),
      };
      journal = { ...journal, failure };
      journal = appendWindowsUpdateProgress(
        journal,
        "rollback-started",
        "crash recovery entered inverse replay before further forward mutation",
        {
          state: "rolling-back",
          error: failure.error,
          now: now(deps),
        },
      );
      await deps.store.save(journal);
      return await rollback(journal, deps);
    }
    if (
      journal.stableArtifacts.mode === "promote"
      && before(journal, "services-stopped")
    ) {
      await ensurePreStopCurrentManifestV2(journal, deps);
    }
    if (journal.state === "rolling-back") return await rollback(journal, deps);

    try {
      if (journal.phase === "prepared") {
        journal = await checkpoint(
          journal,
          deps,
          "broker-started",
          "SCM-launched updater resumed the durable journal under the machine lease",
        );
      }
      if (before(journal, "assets-staged")) {
        if (journal.stableArtifacts.mode === "promote") {
          await stageAndVerifyAssets(journal, deps.native);
        }
        journal = await checkpoint(
          journal,
          deps,
          "assets-staged",
          journal.stableArtifacts.mode === "proof-only"
            ? "protected current manifest supplies the exact signed same-release identity"
            : "manifest, package, release tree, digests, and signatures verified",
        );
      }
      if (before(journal, "stable-artifacts-snapshotted")) {
        journal = await snapshotStableArtifacts(journal, deps);
        journal = await checkpoint(
          journal,
          deps,
          "stable-artifacts-snapshotted",
          "exact prior stable bytes, digests, and security descriptors durably captured",
        );
      }
      if (before(journal, "services-stopped")) {
        const stopped: RoostServiceRole[] = [];
        if (journal.stableArtifacts.mode === "promote") {
          for (const role of STOP_ORDER) {
            if ((await deps.services.query(role)).state !== "running") continue;
            await deps.services.stop(role, { timeoutMs: 15_000 });
            stopped.push(role);
          }
        }
        journal = { ...journal, stoppedRoles: uniqueRoles([...journal.stoppedRoles, ...stopped]) };
        journal = await checkpoint(
          journal,
          deps,
          "services-stopped",
          journal.stableArtifacts.mode === "proof-only"
            ? "proof-only transaction left service lifecycle untouched"
            : stopped.length
              ? `stopped roles before stable cutover: ${stopped.join(", ")}`
              : "all mutable roles were already stopped",
        );
      }
      if (before(journal, "stable-artifacts-promoted")) {
        await promoteStableArtifacts(journal, deps);
        journal = await checkpoint(
          journal,
          deps,
          "stable-artifacts-promoted",
          "verified Shawl and launch-current binaries promoted by constrained native replacement",
        );
      }
      if (before(journal, "updater-config-switched")) {
        if (journal.stableArtifacts.mode === "promote") {
          const updater = journal.nextServiceDefinitions.find(
            (definition) => definition.role === "updater",
          );
          if (!updater) throw new Error("next updater service definition is missing");
          await deps.services.install(updater);
          await writeServiceDefinitions(
            journal.paths.currentManifestPath,
            journal.nextServiceDefinitions,
          );
        }
        journal = await checkpoint(
          journal,
          deps,
          "updater-config-switched",
          journal.stableArtifacts.mode === "proof-only"
            ? "proof-only transaction preserved the immutable updater target"
            : "updater SCM target switched to the signed immutable release for restart recovery",
        );
      }
      if (before(journal, "current-manifest-switched")) {
        if (journal.stableArtifacts.mode === "proof-only") {
          const actual = await readCurrentManifestHeld(
            journal.paths.currentManifestPath,
            deps,
          );
          if (actual !== `${JSON.stringify(journal.currentManifest.next)}\n`) {
            throw new Error("same-digest current manifest bytes are not exact");
          }
        } else {
          await writeCurrentManifest(
            journal.paths.currentManifestPath,
            `${JSON.stringify(journal.currentManifest.next)}\n`,
            deps,
          );
        }
        journal = await checkpoint(
          journal,
          deps,
          "current-manifest-switched",
          journal.stableArtifacts.mode === "proof-only"
            ? "proof-only transaction verified current manifest without mutation"
            : "current manifest switched durably after stable promotion",
        );
      }
      if (before(journal, "services-restored")) {
        const roles = await startDesiredRoles(journal, deps);
        journal = { ...journal, restoredRoles: uniqueRoles([...journal.restoredRoles, ...roles]) };
        journal = await checkpoint(
          journal,
          deps,
          "services-restored",
          journal.stableArtifacts.mode === "proof-only"
            ? "proof-only transaction preserved desired lifecycle"
            : `restored desired lifecycle: ${roles.join(", ") || "no active roles"}`,
        );
      }
      if (before(journal, "health-proven")) {
        await proveExactForwardState(journal, deps);
        journal = await checkpoint(
          journal,
          deps,
          "health-proven",
          "exact stable topology, SCM security, lifecycle, build health, and fresh keeper proven",
        );
      }
      if (before(journal, "committed")) {
        journal = await checkpoint(
          journal,
          deps,
          "committed",
          `Windows update to ${journal.targetVersion} durably committed`,
        );
      }
      if (before(journal, "cleanup-complete")) {
        await cleanupForwardArtifacts(journal, deps.services, deps.native);
        journal = appendWindowsUpdateProgress(
          journal,
          "cleanup-complete",
          journal.stableArtifacts.mode === "proof-only"
            ? "same-digest topology, lifecycle, health, build, stable artifacts, current security, and keeper proven"
            : "post-commit backups, staged admission assets, and unreferenced prior release removed",
          { state: "succeeded", terminal: true, success: true, now: now(deps) },
        );
        await deps.store.save(journal);
      }
      return journal;
    } catch (error) {
      // The durable commit point is irreversible; only post-commit cleanup may replay.
      if (phaseAtLeast(journal.phase, "committed")) {
        journal = appendWindowsUpdateProgress(
          journal,
          journal.phase,
          "post-commit cleanup interrupted; SCM restart will retry cleanup forward",
          { state: "forward", error: errorText(error), now: now(deps) },
        );
        await deps.store.save(journal);
        throw error;
      }
      const forwardPhase = isForward(journal.phase) ? journal.phase : "broker-started";
      journal = {
        ...journal,
        failure: { forwardPhase, error: errorText(error), at: now(deps).toISOString() },
      };
      journal = appendWindowsUpdateProgress(
        journal,
        "rollback-started",
        `forward update failed at ${forwardPhase}; rolling back`,
        { state: "rolling-back", error: errorText(error), now: now(deps) },
      );
      await deps.store.save(journal);
      return await rollback(journal, deps);
    }
  } finally {
    await transaction.release();
  }
}

async function migrateLegacyJournal(
  legacy: WindowsUpdateJournalV1,
  _deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2> {
  throw new Error(
    `legacy Windows update journal ${legacy.jobId} requires signed elevated installer migration before mutation`,
  );
}

function assertLegacyNextManifest(
  next: WindowsCurrentManifestV1,
  legacy: WindowsUpdateJournalV1,
  manifest: ReturnType<typeof parseWindowsReleaseManifest>,
  newVersionDir: string,
  diagnosis: (reason: string) => Error,
): void {
  if (
    next.version !== manifest.version
    || (next.build !== undefined && next.build !== manifest.build)
    || !samePath(next.versionDir, newVersionDir)
    || next.manifestUrl !== legacy.signedManifest.url
    || next.manifestSha256 !== legacy.signedManifest.sha256
    || next.publisherSha256 !== legacy.signedManifest.publisherSha256
    || JSON.stringify(next.files)
      !== JSON.stringify(manifest.files.map(({ path, sha256, size }) => ({ path, sha256, size })))
  ) {
    throw diagnosis("the legacy next current manifest disagrees with the CMS-authenticated target");
  }
}

async function readCurrentManifestHeld(
  path: string,
  deps: WindowsUpdateBrokerDeps,
): Promise<string> {
  if (deps.native.readArtifact) {
    const bytes = await deps.native.readArtifact(path, "current", 16 * 1024 * 1024);
    return Buffer.from(bytes).toString("utf8");
  }
  if ((deps.platform ?? process.platform) === "win32") {
    throw new Error("held Windows current-manifest read is unavailable");
  }
  return await readFile(path, "utf8");
}

async function ensurePreStopCurrentManifestV2(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (journal.currentManifest.priorRaw === null) return;
  const actualRaw = await readCurrentManifestHeld(journal.paths.currentManifestPath, deps);
  if (actualRaw === journal.currentManifest.priorRaw) return;
  const actual = parseWindowsCurrentManifest(actualRaw);
  const expected = parseWindowsCurrentManifest(journal.currentManifest.priorRaw);
  if (
    actual.schemaVersion !== 1
    || expected.schemaVersion !== 2
    || actual.version !== expected.version
    || !samePath(actual.versionDir, expected.versionDir)
    || actual.manifestSha256 !== expected.manifestSha256
    || actual.publisherSha256 !== expected.publisherSha256
    || JSON.stringify(actual.files) !== JSON.stringify(expected.files)
    || (actual.build !== undefined && actual.build !== expected.build)
  ) {
    throw new Error("pre-stop current manifest changed after journal preparation; refusing service mutation");
  }
  await writeCurrentManifest(journal.paths.currentManifestPath, journal.currentManifest.priorRaw, deps);
}

async function assertJournalPathBindings(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  const roots = brokerRoots(deps);
  const stagingDir = join(
    roots.serviceDir,
    "updates",
    createHash("sha256").update(journal.jobId).digest("hex"),
  );
  const manifestPath = join(stagingDir, "roost-windows-x64.manifest.json");
  const signaturePath = `${manifestPath}.p7s`;
  const packagePath = join(stagingDir, "roost-windows-x64.zip");
  const newVersionDir = join(roots.versionsDir, journal.targetVersion);
  requireSamePath(journal.paths.stagingDir, stagingDir, "journal staging directory");
  requireSamePath(journal.paths.currentManifestPath, roots.currentManifestPath, "journal current manifest path");
  if (journal.stableArtifacts.mode === "proof-only") {
    if (
      journal.signedManifest.path !== null
      || journal.signedManifest.signaturePath !== null
      || journal.releasePackage !== null
    ) {
      throw new Error("proof-only journal contains fabricated staged evidence");
    }
  } else {
    assertPromotionEvidence(journal);
    requireSamePath(journal.signedManifest.path, manifestPath, "journal signed manifest path");
    requireSamePath(journal.signedManifest.signaturePath, signaturePath, "journal manifest signature path");
    requireSamePath(journal.releasePackage.path, packagePath, "journal release package path");
  }
  requireSamePath(journal.paths.newVersionDir, newVersionDir, "journal target version directory");
  requireSamePath(journal.currentManifest.next.versionDir, newVersionDir, "journal next current version directory");
  if (
    journal.currentManifest.next.version !== journal.targetVersion
    || journal.currentManifest.next.build !== journal.targetBuild
    || journal.currentManifest.next.manifestUrl !== journal.signedManifest.url
    || journal.currentManifest.next.manifestSha256 !== journal.signedManifest.sha256
    || journal.currentManifest.next.publisherSha256 !== journal.signedManifest.publisherSha256
  ) {
    throw new Error("journal target metadata is not internally bound");
  }
  assertStableWindowsUpdateTopology(
    journal.priorServiceDefinitions,
    journal.nextServiceDefinitions,
    roots.serviceDir,
    journal.paths.newVersionDir,
  );

  if (journal.currentManifest.priorRaw === null) {
    if (journal.paths.priorVersionDir !== null) {
      throw new Error("journal prior version directory exists without a prior current manifest");
    }
  } else {
    const prior = parseWindowsCurrentManifest(journal.currentManifest.priorRaw);
    if (prior.schemaVersion !== 2) throw new Error("journal prior current manifest is not schema 2");
    const priorVersionDir = join(roots.versionsDir, prior.version);
    requireSamePath(prior.versionDir, priorVersionDir, "journal prior current version directory");
    requireSamePath(journal.paths.priorVersionDir, priorVersionDir, "journal prior version directory");
    await assertNoReparseComponents(roots.versionsDir, priorVersionDir);
  }
  await assertNoReparseComponents(roots.serviceDir, roots.currentManifestPath);
  if (journal.stableArtifacts.mode === "promote") {
    await assertNoReparseComponents(roots.serviceDir, manifestPath);
    await assertNoReparseComponents(roots.serviceDir, signaturePath);
    await assertNoReparseComponents(roots.serviceDir, packagePath);
  }
  await assertNoReparseComponents(roots.versionsDir, newVersionDir);
}

export function assertStableWindowsUpdateTopology(
  prior: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
  next: readonly WindowsServiceDefinition[],
  serviceDir: string,
  expectedUpdaterDir?: string,
): void {
  const installRoot = dirname(serviceDir);
  const shawlPath = join(installRoot, "bin", "shawl.exe");
  const launcherPath = join(installRoot, "bin", "roost.exe");
  const expectedArguments: Readonly<Record<RoostServiceRole, readonly string[]>> = {
    keeper: ["keeper", "--service"],
    worker: ["worker"],
    coordinator: ["coord"],
    updater: ["__windows-updater-broker"],
  };
  const nextByRole = Object.fromEntries(
    next.map((entry) => [entry.role, entry]),
  ) as Partial<Record<RoostServiceRole, WindowsServiceDefinition>>;
  for (const role of SERVICE_ROLES) {
    const before = prior[role];
    const after = nextByRole[role];
    if (!before || !after) {
      throw new Error(
        "installed Windows service topology is incomplete; rerun the signed elevated installer",
      );
    }
    if (role === "updater") {
      const priorExpected = retargetWindowsUpdaterDefinition(
        before,
        dirname(before.executablePath),
      );
      const nextExpected = retargetWindowsUpdaterDefinition(
        before,
        expectedUpdaterDir ?? dirname(before.executablePath),
      );
      if (
        JSON.stringify(before) !== JSON.stringify(priorExpected)
        || JSON.stringify(after) !== JSON.stringify(nextExpected)
      ) {
        throw new Error(
          "installed Windows updater is not pinned to the expected immutable release; "
            + "rerun the signed elevated installer to migrate before updating",
        );
      }
      continue;
    }
    const separator = before.shawlArguments.indexOf("--");
    const expectedImagePath =
      [before.shawlPath, ...before.shawlArguments].map(quoteWindowsArg).join(" ");
    if (
      !samePath(before.shawlPath, shawlPath)
      || !samePath(before.serviceLauncherPath, launcherPath)
      || separator < 0
      || !samePath(before.shawlArguments[separator + 1] ?? "", launcherPath)
      || before.shawlArguments[separator + 2] !== "launch-current"
      || JSON.stringify(before.shawlArguments.slice(separator + 3))
        !== JSON.stringify(expectedArguments[role])
      || JSON.stringify(before.arguments) !== JSON.stringify(expectedArguments[role])
      || Object.keys(before.environment).some((key) => key.toUpperCase() === "ROOST_WIN_HELPER")
      || before.imagePath !== expectedImagePath
      || JSON.stringify(after) !== JSON.stringify(before)
    ) {
      throw new Error(
        "installed Windows service topology is not the stable launch-current topology; "
          + "rerun the signed elevated installer to migrate before updating",
      );
    }
  }
}

async function rollback(
  initial: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2> {
  if (initial.stableArtifacts.mode === "proof-only") {
    const terminal = appendWindowsUpdateProgress(
      initial,
      "rolled-back",
      "same-digest proof failed without mutating the machine",
      {
        state: "rolled-back",
        terminal: true,
        success: false,
        error: initial.failure?.error,
        now: now(deps),
      },
    );
    await deps.store.save(terminal);
    await removeAdmissionMarker(terminal);
    return terminal;
  }
  let journal = initial;
  try {
    const touched = phaseAtLeast(
      journal.failure?.forwardPhase ?? "prepared",
      "stable-artifacts-snapshotted",
    );
    if (rollbackBefore(journal, "rollback-services-stopped")) {
      if (touched) {
        for (const role of STOP_ORDER) {
          if ((await deps.services.query(role)).state === "running") {
            await deps.services.stop(role, { timeoutMs: 15_000 });
          }
        }
      }
      journal = await checkpoint(
        journal,
        deps,
        "rollback-services-stopped",
        touched ? "stopped mutable roles before inverse replay" : "forward mutation never began",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-stable-artifacts-restored")) {
      if (touched) await restoreStableArtifacts(journal, deps);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-stable-artifacts-restored",
        touched ? "restored exact prior stable bytes and full DACLs" : "stable artifacts were untouched",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-configs-restored")) {
      await restorePriorServiceConfiguration(journal, deps.services);
      await writeServiceDefinitions(
        journal.paths.currentManifestPath,
        Object.values(journal.priorServiceDefinitions),
      );
      await rollbackVersionAssets(journal);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-configs-restored",
        "restored exact prior SCM configuration and security",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-current-manifest-restored")) {
      await restoreCurrentManifest(journal, deps);
      journal = await checkpoint(
        journal,
        deps,
        "rollback-current-manifest-restored",
        "restored exact prior current manifest bytes",
        "rolling-back",
      );
    }
    if (rollbackBefore(journal, "rollback-services-restored")) {
      const roles = await restorePriorLifecycle(journal, deps.services);
      for (const role of ACTIVE_ROLES) {
        if (journal.runningBefore[role]) await deps.health.prove(role, journal, "rollback");
      }
      journal = { ...journal, restoredRoles: uniqueRoles([...journal.restoredRoles, ...roles]) };
      journal = await checkpoint(
        journal,
        deps,
        "rollback-services-restored",
        "restored and proved the exact prior lifecycle and build vector",
        "rolling-back",
      );
    }
    journal = appendWindowsUpdateProgress(
      journal,
      "rolled-back",
      "Windows update rolled back deterministically",
      {
        state: "rolled-back",
        terminal: true,
        success: false,
        error: journal.failure?.error,
        now: now(deps),
      },
    );
    await deps.store.save(journal);
    await removeAdmissionMarker(journal);
    return journal;
  } catch (error) {
    const phase = isRollback(journal.phase) ? journal.phase : "rollback-started";
    journal = {
      ...journal,
      rollbackFailure: { phase, error: errorText(error), at: now(deps).toISOString() },
    };
    journal = appendWindowsUpdateProgress(
      journal,
      phase,
      "rollback interrupted; SCM restart will resume this journal",
      { state: "rolling-back", error: errorText(error), now: now(deps) },
    );
    await deps.store.save(journal);
    throw error;
  }
}

type StableArtifactName = "shawl" | "launcher";
type StableArtifactEntry = readonly [
  StableArtifactName,
  WindowsStableArtifactPlan,
  Extract<WindowsUpdaterPersistenceProfile, "stable-shawl" | "stable-launcher">,
];

function stableArtifactEntries(journal: WindowsUpdateJournalV2): readonly StableArtifactEntry[] {
  return [
    ["shawl", journal.stableArtifacts.shawl, "stable-shawl"],
    ["launcher", journal.stableArtifacts.launcher, "stable-launcher"],
  ];
}

function nativeTransactionOps(native: WindowsUpdateNative): {
  copyArtifact(
    sourcePath: string,
    destinationPath: string,
    sourceProfile: WindowsUpdaterPersistenceProfile,
    destinationProfile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }>;
  inspectArtifact(
    path: string,
    profile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }>;
} {
  if (native.copyArtifact && native.inspectArtifact) {
    return {
      copyArtifact: native.copyArtifact.bind(native),
      inspectArtifact: native.inspectArtifact.bind(native),
    };
  }
  if (process.platform === "win32") {
    throw new Error("Windows updater native held-handle transaction operations are unavailable");
  }
  const inspect = async (
    path: string,
    _profile: WindowsUpdaterPersistenceProfile,
    expected?: { sha256: string; size: number },
  ): Promise<{ sha256: string; size: number; sddl: string }> => {
    const bytes = await readFile(path);
    const actual = {
      sha256: sha256Hex(bytes),
      size: bytes.byteLength,
      sddl: "non-windows-test-security-descriptor",
    };
    if (expected && (actual.sha256 !== expected.sha256 || actual.size !== expected.size)) {
      throw new Error(`artifact source does not match expected identity: ${path}`);
    }
    return actual;
  };
  return {
    inspectArtifact: inspect,
    copyArtifact: async (sourcePath, destinationPath, sourceProfile, destinationProfile, expected) => {
      const actual = await inspect(sourcePath, sourceProfile, expected);
      await replaceWindowsUpdaterArtifact(destinationPath, await readFile(sourcePath), destinationProfile);
      return actual;
    },
  };
}

async function snapshotStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<WindowsUpdateJournalV2> {
  if (journal.stableArtifacts.mode === "proof-only") {
    await verifyStableArtifacts(journal, deps);
    return journal;
  }
  const native = nativeTransactionOps(deps.native);
  const captured: Partial<Record<StableArtifactName, WindowsStableArtifactPlan>> = {};
  for (const [name, plan, profile] of stableArtifactEntries(journal)) {
    const source = await native.inspectArtifact(plan.stablePath, profile);
    if (!source.sddl.trim()) throw new Error(`${name} held stable security proof is empty`);
    const expected = { sha256: source.sha256, size: source.size };
    const backupPath = join(journal.stableArtifacts.backupDir, `${name}.bak`);
    let backupReady = false;
    try {
      await native.inspectArtifact(backupPath, "private", expected);
      backupReady = true;
    } catch {
      // Missing is the ordinary first-pass case. Copy is constrained and the
      // post-failure inspect closes the crash/race gap if another pass won.
    }
    if (!backupReady) {
      try {
        const copied = await native.copyArtifact(
          plan.stablePath,
          backupPath,
          profile,
          "private",
          expected,
        );
        if (
          copied.sha256 !== source.sha256
          || copied.size !== source.size
          || normalizeSddl(copied.sddl) !== normalizeSddl(source.sddl)
        ) {
          throw new Error(`${name} stable backup source proof changed during copy`);
        }
      } catch (copyError) {
        try {
          await native.inspectArtifact(backupPath, "private", expected);
        } catch {
          throw copyError;
        }
      }
    }
    const sourceAfter = await native.inspectArtifact(plan.stablePath, profile, expected);
    if (normalizeSddl(sourceAfter.sddl) !== normalizeSddl(source.sddl)) {
      throw new Error(`${name} stable owner/DACL changed while snapshotting`);
    }
    captured[name] = {
      ...plan,
      prior: {
        existed: true,
        backupPath,
        sha256: source.sha256,
        size: source.size,
        securityDescriptor: source.sddl,
      },
    };
  }
  return {
    ...journal,
    stableArtifacts: {
      ...journal.stableArtifacts,
      shawl: captured.shawl!,
      launcher: captured.launcher!,
    },
  };
}

async function promoteStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (journal.stableArtifacts.mode === "proof-only") {
    await verifyStableArtifacts(journal, deps);
    return;
  }
  const native = nativeTransactionOps(deps.native);
  for (const [, plan, profile] of stableArtifactEntries(journal)) {
    await native.copyArtifact(
      plan.releasePath,
      plan.stablePath,
      "release",
      profile,
      { sha256: plan.sha256, size: plan.size },
    );
    await verifyFile(plan.stablePath, plan.sha256, plan.size);
    await deps.native.verifyAuthenticode(plan.stablePath, journal.signedManifest.publisherSha256);
  }
}

async function restoreStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (journal.stableArtifacts.mode === "proof-only") return;
  const native = nativeTransactionOps(deps.native);
  for (const [name, plan, profile] of stableArtifactEntries(journal)) {
    if (!plan.prior || !plan.prior.existed) {
      throw new Error(`cannot restore ${name}: durable prior stable snapshot is missing`);
    }
    await native.copyArtifact(
      plan.prior.backupPath,
      plan.stablePath,
      "private",
      profile,
      { sha256: plan.prior.sha256, size: plan.prior.size },
    );
    const restored = await native.inspectArtifact(
      plan.stablePath,
      profile,
      { sha256: plan.prior.sha256, size: plan.prior.size },
    );
    if (normalizeSddl(restored.sddl) !== normalizeSddl(plan.prior.securityDescriptor)) {
      throw new Error(`${name} stable DACL did not restore exactly`);
    }
  }
}

async function verifyStableArtifacts(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  const native = nativeTransactionOps(deps.native);
  for (const [, plan, profile] of stableArtifactEntries(journal)) {
    await native.inspectArtifact(
      plan.stablePath,
      profile,
      { sha256: plan.sha256, size: plan.size },
    );
    await deps.native.verifyAuthenticode(plan.stablePath, journal.signedManifest.publisherSha256);
  }
}


function normalizeSddl(value: string): string {
  return value.replace(/\s+/g, "").toUpperCase();
}

async function startDesiredRoles(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<RoostServiceRole[]> {
  if (journal.stableArtifacts.mode === "proof-only") return [];
  const started: RoostServiceRole[] = [];
  for (const role of START_ORDER) {
    if (!journal.desiredRunning[role]) continue;
    const result = await deps.services.start(role);
    if (result.state !== "running") throw new Error(`${role} did not reach desired running state`);
    if (role === "keeper") {
      const executable = journal.assets.find(({ path }) => path.toLowerCase() === "roost.exe");
      if (!executable) throw new Error("signed release omits the keeper executable");
      const target = resolveUnder(journal.paths.newVersionDir, executable.path);
      if (await deps.native.probeExclusiveOpen(target)) {
        throw new Error("keeper did not hold the target release executable after restart");
      }
    }
    started.push(role);
  }
  return started;
}

async function proveExactForwardState(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  await verifyStableArtifacts(journal, deps);
  await verifyTree(
    journal.paths.newVersionDir,
    journal.assets,
    journal.signedManifest.publisherSha256,
    deps.native,
  );
  assertStableWindowsUpdateTopology(
    journal.priorServiceDefinitions,
    journal.nextServiceDefinitions,
    dirname(journal.paths.currentManifestPath),
    journal.paths.newVersionDir,
  );
  const installedDefinitions = await readFile(
    join(dirname(journal.paths.currentManifestPath), "service-definitions.json"),
    "utf8",
  );
  if (installedDefinitions !== serviceDefinitionsDocument(journal.nextServiceDefinitions)) {
    throw new Error("protected Windows service definitions do not match the committed updater target");
  }
  for (const role of SERVICE_ROLES) {
    const actual = await deps.services.query(role, { includeSecurity: true });
    const definition = journal.nextServiceDefinitions.find((entry) => entry.role === role);
    if (!definition) throw new Error(`next ${role} service definition is missing`);
    assertSameServiceConfiguration(
      expectedSnapshotForDefinition(journal.serviceSnapshot[role], definition),
      actual,
    );
    if (role !== "updater") {
      const expected = journal.desiredRunning[role] ? "running" : "stopped";
      if (actual.state !== expected) {
        throw new Error(`${role} lifecycle is ${actual.state}, expected ${expected}`);
      }
    }
  }
  if (
    journal.stableArtifacts.mode === "promote"
    && journal.desiredRunning.keeper
    && !journal.restoredRoles.includes("keeper")
  ) {
    throw new Error("keeper was not freshly launched through the promoted stable topology");
  }
  for (const role of ACTIVE_ROLES) {
    if (journal.desiredRunning[role]) {
      await deps.health.prove(
        role,
        journal,
        journal.stableArtifacts.mode === "proof-only" ? "proof" : "forward",
      );
    }
  }
  if (journal.desiredRunning.keeper) {
    const executable = journal.assets.find(({ path }) => path.toLowerCase() === "roost.exe");
    if (!executable) throw new Error("signed release omits the keeper executable");
    const target = resolveUnder(journal.paths.newVersionDir, executable.path);
    if (await deps.native.probeExclusiveOpen(target)) {
      throw new Error("keeper is not holding the signed active release executable");
    }
  }
  const currentRaw = await readCurrentManifestHeld(journal.paths.currentManifestPath, deps);
  if (currentRaw !== `${JSON.stringify(journal.currentManifest.next)}\n`) {
    throw new Error("current manifest bytes changed before commit");
  }
  await assertCurrentManifestSecurity(
    journal,
    deps,
    `${JSON.stringify(journal.currentManifest.next)}\n`,
  );
}

function assertSameServiceConfiguration(
  expected: WindowsServiceSnapshot,
  actual: WindowsServiceSnapshot,
): void {
  const same = expected.installed === actual.installed
    && expected.imagePath === actual.imagePath
    && expected.startMode === actual.startMode
    && expected.account?.trim().toLowerCase() === actual.account?.trim().toLowerCase()
    && JSON.stringify(expected.dependencies) === JSON.stringify(actual.dependencies)
    && expected.displayName === actual.displayName
    && expected.description === actual.description
    && JSON.stringify(expected.recoveryPolicy) === JSON.stringify(actual.recoveryPolicy)
    && JSON.stringify(expected.environment) === JSON.stringify(actual.environment)
    && expected.serviceSidType === actual.serviceSidType
    && (
      expected.securityDescriptor === undefined
      || (
        expected.securityDescriptor === null
          ? actual.securityDescriptor === null
          : (
            typeof actual.securityDescriptor === "string"
            && normalizeSddl(expected.securityDescriptor) === normalizeSddl(actual.securityDescriptor)
          )
      )
    );
  if (!same) throw new Error(`${expected.name} SCM configuration or security changed`);
}
function expectedSnapshotForDefinition(
  baseline: WindowsServiceSnapshot,
  definition: WindowsServiceDefinition,
): WindowsServiceSnapshot {
  return {
    ...baseline,
    installed: true,
    imagePath: definition.imagePath,
    startMode: definition.startMode,
    account: definition.account,
    dependencies: definition.dependencies.map((role) => WINDOWS_SERVICE_NAMES[role]),
    displayName: definition.displayName,
    description: definition.description,
    environment: definition.environment,
  };
}
function serviceDefinitionsDocument(
  definitions: readonly WindowsServiceDefinition[],
): string {
  const byRole = Object.fromEntries(
    definitions.map((definition) => [definition.role, definition]),
  ) as Partial<Record<RoostServiceRole, WindowsServiceDefinition>>;
  const services = Object.fromEntries(SERVICE_ROLES.map((role) => {
    const definition = byRole[role];
    if (!definition) throw new Error(`Windows service definitions omit ${role}`);
    return [role, definition];
  })) as Record<RoostServiceRole, WindowsServiceDefinition>;
  return `${JSON.stringify({ schemaVersion: 2, services }, null, 2)}\n`;
}

async function writeServiceDefinitions(
  currentManifestPath: string,
  definitions: readonly WindowsServiceDefinition[],
): Promise<void> {
  await replaceWindowsUpdaterArtifact(
    join(dirname(currentManifestPath), "service-definitions.json"),
    serviceDefinitionsDocument(definitions),
    "control",
  );
}



async function restorePriorServiceConfiguration(
  journal: WindowsUpdateJournalV2,
  services: WindowsUpdateServiceManager,
): Promise<void> {
  if (!services.restore) {
    if (process.platform === "win32") throw new Error("exact Windows SCM restore operation is unavailable");
    return;
  }
  await services.restore(journal.serviceSnapshot, {
    restoreLifecycleRoles: [],
    allowKeeperStop: true,
  });
}

async function restorePriorLifecycle(
  journal: WindowsUpdateJournalV2,
  services: WindowsUpdateServiceManager,
): Promise<RoostServiceRole[]> {
  if (services.restore) {
    await services.restore(journal.serviceSnapshot, {
      restoreLifecycleRoles: START_ORDER,
      allowKeeperStop: true,
    });
    return START_ORDER.filter((role) => journal.runningBefore[role]);
  }
  const restored: RoostServiceRole[] = [];
  for (const role of START_ORDER) {
    const actual = await services.query(role);
    if (journal.runningBefore[role] && actual.state !== "running") {
      await services.start(role);
      restored.push(role);
    } else if (!journal.runningBefore[role] && actual.state === "running") {
      await services.stop(role, { timeoutMs: 15_000 });
    }
  }
  return restored;
}

function assertPromotionEvidence(
  journal: WindowsUpdateJournalV2,
): asserts journal is WindowsUpdateJournalV2 & {
  signedManifest: WindowsUpdateJournalV2["signedManifest"] & {
    path: string;
    signaturePath: string;
  };
  releasePackage: NonNullable<WindowsUpdateJournalV2["releasePackage"]>;
} {
  if (
    journal.stableArtifacts.mode !== "promote"
    || journal.signedManifest.path === null
    || journal.signedManifest.signaturePath === null
    || journal.releasePackage === null
  ) {
    throw new Error("promotion journal lacks signed staged evidence");
  }
}

async function stageAndVerifyAssets(
  journal: WindowsUpdateJournalV2,
  native: WindowsUpdateNative,
): Promise<void> {
  assertPromotionEvidence(journal);
  const raw = await readFile(journal.signedManifest.path);
  if (sha256Hex(raw) !== journal.signedManifest.sha256) {
    throw new Error("staged manifest digest changed");
  }
  await native.verifyCmsDetached(
    journal.signedManifest.path,
    journal.signedManifest.signaturePath,
    journal.signedManifest.publisherSha256,
  );
  const manifest = parseWindowsReleaseManifest(raw);
  if (
    manifest.version !== journal.targetVersion
    || manifest.build !== journal.targetBuild
    || manifest.package.sha256 !== journal.releasePackage.sha256
    || manifest.package.size !== journal.releasePackage.size
    || JSON.stringify(manifest.files) !== JSON.stringify(journal.assets)
  ) {
    throw new Error("journal does not match signed manifest");
  }
  await verifyFile(journal.releasePackage.path, journal.releasePackage.sha256, journal.releasePackage.size);
  if (await directoryExists(journal.paths.newVersionDir)) {
    await verifyTree(
      journal.paths.newVersionDir,
      journal.assets,
      journal.signedManifest.publisherSha256,
      native,
    );
    return;
  }
  const versionParent = dirname(journal.paths.newVersionDir);
  await mkdir(versionParent, { recursive: true });
  const extracted = join(
    versionParent,
    `.extracting-${basename(journal.paths.newVersionDir)}-${process.pid}-${randomUUID()}`,
  );
  let installed = false;
  try {
    await native.extractZip(journal.releasePackage.path, extracted, journal.assets);
    await verifyTree(extracted, journal.assets, journal.signedManifest.publisherSha256, native);
    await durableReplace(extracted, journal.paths.newVersionDir);
    installed = true;
  } finally {
    if (!installed) await rm(extracted, { recursive: true, force: true }).catch(() => undefined);
  }
  await flushDurablePath(versionParent);
  await verifyTree(
    journal.paths.newVersionDir,
    journal.assets,
    journal.signedManifest.publisherSha256,
    native,
  );
}

async function verifyTree(
  root: string,
  assets: readonly WindowsReleaseFile[],
  publisher: string,
  native: WindowsUpdateNative,
): Promise<void> {
  const expected = new Set(assets.map((asset) => asset.path.replaceAll("\\", "/").toLowerCase()));
  const actual = await listFiles(root);
  for (const path of actual) {
    if (!expected.has(path.toLowerCase())) throw new Error(`unmanifested archive asset: ${path}`);
  }
  if (actual.length !== expected.size) throw new Error("archive asset count mismatch");
  const held = nativeTransactionOps(native);
  for (const asset of assets) {
    const path = resolveUnder(root, asset.path);
    await held.inspectArtifact(
      path,
      "release",
      { sha256: asset.sha256, size: asset.size },
    );
    if (asset.authenticodeRequired) await native.verifyAuthenticode(path, publisher);
  }
}

async function verifyFile(path: string, expected: string, bytes: number): Promise<void> {
  const info = await lstat(path);
  if (!info.isFile() || info.isSymbolicLink() || info.size !== bytes) {
    throw new Error(`asset metadata mismatch: ${path}`);
  }
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer);
  if (hash.digest("hex") !== expected) throw new Error(`asset digest mismatch: ${path}`);
}

async function listFiles(root: string, prefix = ""): Promise<string[]> {
  const result: string[] = [];
  for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) {
    const path = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isSymbolicLink()) throw new Error(`archive link/reparse asset: ${path}`);
    if (entry.isDirectory()) result.push(...await listFiles(root, path));
    else if (entry.isFile()) result.push(path);
    else throw new Error(`unsupported archive asset: ${path}`);
  }
  return result;
}

async function cleanupForwardArtifacts(
  journal: WindowsUpdateJournalV2,
  services: WindowsUpdateServiceManager,
  native: WindowsUpdateNative,
): Promise<void> {
  if (journal.stableArtifacts.mode === "proof-only") return;
  await rm(journal.paths.stagingDir, { recursive: true, force: true });
  await flushDurablePath(dirname(journal.paths.stagingDir));
  await removeAdmissionMarker(journal);
  await rm(journal.stableArtifacts.backupDir, { recursive: true, force: true });
  await flushDurablePath(dirname(journal.stableArtifacts.backupDir));
  const prior = journal.paths.priorVersionDir;
  if (!prior || samePath(prior, journal.paths.newVersionDir) || !await directoryExists(prior)) return;
  const folded = prior.replaceAll("/", "\\").toLowerCase();
  for (const role of SERVICE_ROLES) {
    const imagePath = (await services.query(role)).imagePath?.replaceAll("/", "\\").toLowerCase();
    if (imagePath?.includes(folded)) {
      throw new Error(`${role} still references the prior Windows release`);
    }
  }
  if (process.execPath.replaceAll("/", "\\").toLowerCase().startsWith(`${folded}\\`)) {
    throw new Error("the prior updater release is still loaded; SCM restart must finish cleanup");
  }
  if (!await native.probeExclusiveOpen(join(prior, "roost.exe"))) {
    throw new Error("the prior Windows release is still in use");
  }
  await rm(prior, { recursive: true, force: true });
  await flushDurablePath(dirname(prior));
}

async function removeAdmissionMarker(_journal: WindowsUpdateJournalV2): Promise<void> {
  // Pending request capability is consumed immediately after the durable journal save.
}

async function rollbackVersionAssets(journal: WindowsUpdateJournalV2): Promise<void> {
  const versionDir = journal.paths.newVersionDir;
  if (versionDir === journal.paths.priorVersionDir || !await directoryExists(versionDir)) return;
  const folded = versionDir.replaceAll("/", "\\").toLowerCase();
  const referenced = Object.values(journal.serviceSnapshot).some((snapshot) =>
    snapshot.imagePath?.replaceAll("/", "\\").toLowerCase().includes(folded)
  ) || process.execPath.replaceAll("/", "\\").toLowerCase().startsWith(`${folded}\\`);
  if (referenced) return;
  const rollbackId = createHash("sha256").update(journal.transactionId).digest("hex").slice(0, 16);
  const quarantine = join(dirname(versionDir), `.rolled-back-${basename(versionDir)}-${rollbackId}`);
  if (await directoryExists(quarantine)) {
    throw new Error(`rollback quarantine already exists while version remains installed: ${quarantine}`);
  }
  await durableReplace(versionDir, quarantine);
  await flushDurablePath(dirname(versionDir));
}

async function assertCurrentManifestSecurity(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
  raw: string,
): Promise<void> {
  const bytes = Buffer.from(raw);
  const expected = { sha256: sha256Hex(bytes), size: bytes.byteLength };
  const inspected = await nativeTransactionOps(deps.native).inspectArtifact(
    journal.paths.currentManifestPath,
    "current",
    expected,
  );
  if (
    normalizeSddl(inspected.sddl)
      !== normalizeSddl(journal.currentManifestSnapshot.securityDescriptor)
  ) {
    throw new Error("current manifest owner/DACL did not restore exactly");
  }
}

async function restoreCurrentManifest(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  const raw = journal.currentManifest.priorRaw;
  if (raw === null) {
    throw new Error("stable Windows rollback requires prior current manifest bytes");
  }
  await writeCurrentManifest(journal.paths.currentManifestPath, raw, deps);
  await assertCurrentManifestSecurity(journal, deps, raw);
}

async function writeCurrentManifest(
  path: string,
  contents: string | null,
  deps: WindowsUpdateBrokerDeps,
): Promise<void> {
  if (deps.writeCurrentManifest) return await deps.writeCurrentManifest(path, contents);
  if (contents === null) {
    throw new Error("installed stable Windows topology requires a prior current manifest");
  }
  if (deps.native.replaceArtifact) {
    await deps.native.replaceArtifact(path, "current", contents);
    return;
  }
  if (process.platform === "win32") {
    throw new Error("constrained Windows current-manifest replacement is unavailable");
  }
  await replaceWindowsUpdaterArtifact(path, contents, "current");
}



async function checkpoint(
  journal: WindowsUpdateJournalV2,
  deps: WindowsUpdateBrokerDeps,
  phase: Parameters<typeof appendWindowsUpdateProgress>[1],
  message: string,
  state: "forward" | "rolling-back" = "forward",
): Promise<WindowsUpdateJournalV2> {
  const next = appendWindowsUpdateProgress(journal, phase, message, { state, now: now(deps) });
  await deps.store.save(next);
  return next;
}

function resolveUnder(root: string, path: string): string {
  const target = resolve(root, path);
  const rel = relative(resolve(root), target);
  if (!rel || rel.startsWith("..") || isAbsolute(rel) || resolve(root, rel) !== target) {
    throw new Error(`asset escapes version directory: ${path}`);
  }
  return target;
}

async function assertNoReparseComponents(root: string, target: string): Promise<void> {
  if (!samePath(root, target) && !pathIsUnder(root, target)) {
    throw new Error(`journal path escapes its protected root: ${target}`);
  }
  const parts = pathRelative(root, target).split(/[\\/]/).filter(Boolean);
  let cursor = root;
  for (const part of ["", ...parts]) {
    if (part) cursor = pathJoin(cursor, part);
    try {
      const info = await lstat(cursor);
      if (info.isSymbolicLink()) throw new Error(`journal path traverses a link/reparse point: ${cursor}`);
    } catch (error) {
      if (nodeError(error)?.code === "ENOENT") return;
      throw error;
    }
  }
}

function requireSamePath(actual: string | null, expected: string, label: string): void {
  if (actual === null || !samePath(actual, expected)) {
    throw new Error(`${label} is not the canonical protected path`);
  }
}

function samePath(left: string, right: string): boolean {
  if (win32.isAbsolute(left) || win32.isAbsolute(right)) {
    return win32.resolve(left).toLowerCase() === win32.resolve(right).toLowerCase();
  }
  return resolve(left) === resolve(right);
}

function pathIsUnder(root: string, target: string): boolean {
  const rel = pathRelative(root, target);
  return rel.length > 0 && !rel.startsWith("..") && !isPathAbsolute(rel);
}

function pathRelative(root: string, target: string): string {
  return win32.isAbsolute(root) || win32.isAbsolute(target)
    ? win32.relative(win32.resolve(root), win32.resolve(target))
    : relative(resolve(root), resolve(target));
}

function pathJoin(root: string, part: string): string {
  return win32.isAbsolute(root) ? win32.join(root, part) : join(root, part);
}

function isPathAbsolute(path: string): boolean {
  return win32.isAbsolute(path) || isAbsolute(path);
}

async function directoryExists(path: string): Promise<boolean> {
  try {
    return (await lstat(path)).isDirectory();
  } catch (error) {
    if (nodeError(error)?.code === "ENOENT") return false;
    throw error;
  }
}

function brokerRoots(deps: WindowsUpdateBrokerDeps): BrokerRoots {
  const serviceDir = deps.serviceDir ?? roostServiceDir();
  const versionsDir = deps.versionsDir ?? roostVersionsDir();
  return {
    serviceDir,
    versionsDir,
    currentManifestPath: deps.currentManifestPath ?? windowsCurrentManifestPath(serviceDir),
  };
}

function before(journal: WindowsUpdateJournalV2, phase: WindowsUpdateForwardPhase): boolean {
  return isForward(journal.phase)
    && FORWARD_ORDER.indexOf(journal.phase) < FORWARD_ORDER.indexOf(phase);
}

function isLegacyPreStopPhase(
  phase: WindowsUpdateJournalV1["phase"],
): phase is "prepared" | "broker-started" | "assets-staged" {
  return phase === "prepared" || phase === "broker-started" || phase === "assets-staged";
}

function phaseAtLeast(
  phase: WindowsUpdateJournalV2["phase"],
  expected: WindowsUpdateForwardPhase,
): boolean {
  return isForward(phase) && FORWARD_ORDER.indexOf(phase) >= FORWARD_ORDER.indexOf(expected);
}

function rollbackBefore(
  journal: WindowsUpdateJournalV2,
  phase: WindowsUpdateRollbackPhase,
): boolean {
  const currentIndex = ROLLBACK_ORDER.findIndex((candidate) => candidate === journal.phase);
  return currentIndex < ROLLBACK_ORDER.indexOf(phase);
}

function isForward(phase: WindowsUpdateJournalV2["phase"]): phase is WindowsUpdateForwardPhase {
  return FORWARD_ORDER.includes(phase as WindowsUpdateForwardPhase);
}

function isRollback(
  phase: WindowsUpdateJournalV2["phase"],
): phase is WindowsUpdateRollbackPhase {
  return ROLLBACK_ORDER.some((candidate) => candidate === phase);
}

function uniqueRoles(roles: readonly RoostServiceRole[]): RoostServiceRole[] {
  return [...new Set(roles)];
}

function normalizedVersion(version: string): string {
  return version.replace(/^v/, "").split("+")[0];
}

function errorText(error: unknown): string {
  return String(error).replace(/[\r\n]+/g, " ").slice(0, 2048);
}

function now(deps: WindowsUpdateBrokerDeps): Date {
  return (deps.now ?? (() => new Date()))();
}

function nodeError(error: unknown): NodeJS.ErrnoException | null {
  return error instanceof Error && "code" in error ? error as NodeJS.ErrnoException : null;
}

export {
  createWindowsRelocationBrokerDeps,
  prepareWindowsRelocationJournal,
  runWindowsRelocationBroker,
} from "./windows-relocation-broker.ts";
