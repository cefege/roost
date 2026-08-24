import { createHash } from "node:crypto";
import { readFile, rm } from "node:fs/promises";
import { dirname, join, win32 } from "node:path";
import {
  flushDurablePath,
} from "@roost/shared/durability";
import { acquireMachineTransaction } from "../machine-transaction.ts";
import { roostServiceDir, roostVersionsDir } from "@roost/shared/paths";
import type { ServiceHealthRole, ServiceHealthStatusFor } from "@roost/shared/service-health";
import {
  WINDOWS_SERVICE_NAMES,
  quoteWindowsArg,
  retargetWindowsUpdaterDefinition,
} from "../service-ctl.ts";
import type {
  RoostServiceRole,
  WindowsServiceDefinition,
  WindowsServiceManager,
  WindowsServiceSnapshot,
  WindowsServiceSnapshotSet,
} from "../service-ctl.ts";
import {
  appendWindowsUpdateProgress,
  assertWindowsUpdateJournal,
  parseWindowsCurrentManifest,
  parseWindowsReleaseManifest,
  windowsCurrentManifestPath,
} from "./windows-update-journal.ts";
import type {
  WindowsCurrentManifestV1,
  WindowsReleaseFile,
  WindowsUpdateForwardPhase,
  WindowsUpdateJournalStore,
  WindowsUpdateJournalV1,
  WindowsUpdateJournalV2,
  WindowsUpdaterPersistenceProfile,
} from "./windows-update-journal.ts";
import {
  assertNoReparseComponents,
  depsNow,
  errorText,
  isPathAbsolute,
  resolveUnder,
  samePath,
} from "./windows-path-safety.ts";
import {
  ACTIVE_ROLES,
  FORWARD_ORDER,
  checkpoint,
  isForward,
  phaseAtLeast,
  removeAdmissionMarker,
  rollback,
  serviceDefinitionsDocument,
  uniqueRoles,
  writeCurrentManifest,
  writeServiceDefinitions,
  START_ORDER,
  STOP_ORDER,
  assertCurrentManifestSecurity,
} from "./windows-update-rollback.ts";
import {
  assertPromotionEvidence,
  directoryExists,
  nativeTransactionOps,
  stageAndVerifyAssets,
  verifyTree,
} from "./windows-update-assets.ts";
import {
  normalizeSddl,
  promoteStableArtifacts,
  restoreStableArtifacts,
  snapshotStableArtifacts,
  verifyStableArtifacts,
} from "./windows-update-stable-artifacts.ts";

const SERVICE_ROLES = ["keeper", "worker", "coordinator", "updater"] as const satisfies readonly RoostServiceRole[];
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
        at: depsNow(deps).toISOString(),
      };
      journal = { ...journal, failure };
      journal = appendWindowsUpdateProgress(
        journal,
        "rollback-started",
        "crash recovery entered inverse replay before further forward mutation",
        {
          state: "rolling-back",
          error: failure.error,
          now: depsNow(deps),
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
          { state: "succeeded", terminal: true, success: true, now: depsNow(deps) },
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
          { state: "forward", error: errorText(error), now: depsNow(deps) },
        );
        await deps.store.save(journal);
        throw error;
      }
      const forwardPhase = isForward(journal.phase) ? journal.phase : "broker-started";
      journal = {
        ...journal,
        failure: { forwardPhase, error: errorText(error), at: depsNow(deps).toISOString() },
      };
      journal = appendWindowsUpdateProgress(
        journal,
        "rollback-started",
        `forward update failed at ${forwardPhase}; rolling back`,
        { state: "rolling-back", error: errorText(error), now: depsNow(deps) },
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









function requireSamePath(actual: string | null, expected: string, label: string): void {
  if (actual === null || !samePath(actual, expected)) {
    throw new Error(`${label} is not the canonical protected path`);
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



export {
  createWindowsRelocationBrokerDeps,
  prepareWindowsRelocationJournal,
  runWindowsRelocationBroker,
} from "./windows-relocation-broker.ts";
