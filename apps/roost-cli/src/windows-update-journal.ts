import { createHash, randomUUID } from "node:crypto";
import { lstat, mkdir, readFile } from "node:fs/promises";
import { dirname, isAbsolute, join, posix, win32 } from "node:path";
import {
  durableRemove,
  durableWriteFile,
  flushDurablePath,
} from "@roost/shared/durability";
import { roostServiceDir } from "@roost/shared/paths";
import {
  windowsReadUpdaterArtifact,
  windowsReplaceUpdaterArtifact,
} from "@roost/shared/windows-helper";
import type { WindowsUpdaterArtifactProfile } from "@roost/shared/windows-helper";
import type {
  RoostServiceRole,
  WindowsServiceDefinition,
  WindowsServiceSnapshotSet,
} from "./service-ctl.ts";

const WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION = 2 as const;
const WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION = 2 as const;
export const MAX_WINDOWS_UPDATE_PROGRESS = 128;
export const WINDOWS_UPDATE_JOURNAL_FILE = "update-v2.json";
export const WINDOWS_LEGACY_UPDATE_JOURNAL_FILE = "update-v1.json";
export const WINDOWS_UPDATE_STATUS_PREFIX = "status-";
export const WINDOWS_CURRENT_MANIFEST_FILE = "current.json";
const SHA256_RE = /^[0-9a-f]{64}$/;
const BUILD_ID_RE = /^[0-9a-f]{40,64}$/;
const SERVICE_ROLES = ["keeper", "worker", "coordinator", "updater"] as const satisfies readonly RoostServiceRole[];
const ACTIVE_ROLES = ["worker", "coordinator"] as const;

export type WindowsUpdateForwardPhase =
  | "prepared"
  | "broker-started"
  | "assets-staged"
  | "stable-artifacts-snapshotted"
  | "services-stopped"
  | "stable-artifacts-promoted"
  | "updater-config-switched"
  | "current-manifest-switched"
  | "services-restored"
  | "health-proven"
  | "cleanup-complete"
  | "committed";
export type WindowsUpdateRollbackPhase =
  | "rollback-started"
  | "rollback-services-stopped"
  | "rollback-stable-artifacts-restored"
  | "rollback-configs-restored"
  | "rollback-current-manifest-restored"
  | "rollback-services-restored"
  | "rolled-back";
/** Accepted only while parsing schema 1 journals written by the versioned-service updater. */
export type WindowsUpdateLegacyForwardPhase = WindowsUpdateForwardPhase | "service-configs-switched";
export type WindowsUpdatePhase = WindowsUpdateLegacyForwardPhase | WindowsUpdateRollbackPhase;
export type WindowsUpdateState = "forward" | "rolling-back" | "succeeded" | "rolled-back";

export interface WindowsReleaseFile {
  path: string;
  sha256: string;
  size: number;
  authenticodeRequired: boolean;
}

/** Detached CMS covers the exact raw bytes containing this JSON. */
export interface WindowsReleaseManifestV1 {
  schemaVersion: 1;
  version: string;
  build: string;
  platform: "win32";
  arch: "x64";
  publishedAt: string;
  package: { name: "roost-windows-x64.zip"; sha256: string; size: number };
  files: WindowsReleaseFile[];
  shawl: { version: "1.9.0"; upstreamSha256: string };
}

interface WindowsCurrentManifestFields {
  version: string;
  versionDir: string;
  files: Array<Pick<WindowsReleaseFile, "path" | "sha256" | "size">>;
  manifestUrl: string;
  manifestSha256: string;
  publisherSha256: string;
}

/** Legacy current.json did not contain an immutable build identity. */
export interface WindowsCurrentManifestV1 extends WindowsCurrentManifestFields {
  schemaVersion: 1;
  build?: string;
}

export interface WindowsCurrentManifestV2 extends WindowsCurrentManifestFields {
  schemaVersion: 2;
  build: string;
}

export type WindowsCurrentManifest = WindowsCurrentManifestV1 | WindowsCurrentManifestV2;

export interface WindowsUpdateProgressEntry {
  sequence: number;
  at: string;
  phase: WindowsUpdatePhase;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}

export interface WindowsServiceHealthCheckpointV1 {
  version: string;
  build?: string;
  processEpoch: string;
  coordinatorUrl?: string;
}

export interface WindowsServiceHealthCheckpointV2 {
  version: string;
  build: string;
  processEpoch: string;
  coordinatorUrl?: string;
}
export type WindowsStableArtifactSnapshot =
  | { existed: false }
  | {
    existed: true;
    backupPath: string;
    sha256: string;
    size: number;
    securityDescriptor: string;
  };
export interface WindowsCurrentManifestSnapshot {
  sha256: string;
  size: number;
  securityDescriptor: string;
}

export interface WindowsStableArtifactPlan {
  releasePath: string;
  stablePath: string;
  sha256: string;
  size: number;
  prior: WindowsStableArtifactSnapshot | null;
}

export interface WindowsStableArtifacts {
  mode: "promote" | "proof-only";
  backupDir: string;
  shawl: WindowsStableArtifactPlan;
  launcher: WindowsStableArtifactPlan;
}


interface WindowsUpdateJournalFields<
  SchemaVersion extends 1 | 2,
  TargetBuild extends string | undefined,
  CurrentManifestType extends WindowsCurrentManifest,
  HealthCheckpoint extends WindowsServiceHealthCheckpointV1 | WindowsServiceHealthCheckpointV2,
> {
  schemaVersion: SchemaVersion;
  transactionId: string;
  jobId: string;
  createdAt: string;
  updatedAt: string;
  revision: number;
  state: WindowsUpdateState;
  phase: WindowsUpdatePhase;
  targetVersion: string;
  targetBuild: TargetBuild;
  signedManifest: {
    url: string;
    signatureUrl: string;
    path: SchemaVersion extends 2 ? string | null : string;
    signaturePath: SchemaVersion extends 2 ? string | null : string;
    sha256: string;
    publisherSha256: string;
  };
  releasePackage: SchemaVersion extends 2
    ? { url: string; path: string; sha256: string; size: number } | null
    : { url: string; path: string; sha256: string; size: number };
  assets: WindowsReleaseFile[];
  stableArtifacts: SchemaVersion extends 2 ? WindowsStableArtifacts : never;
  paths: {
    priorVersionDir: string | null;
    newVersionDir: string;
    stagingDir: string;
    currentManifestPath: string;
  };
  currentManifest: { priorRaw: string | null; next: CurrentManifestType };
  currentManifestSnapshot: SchemaVersion extends 2 ? WindowsCurrentManifestSnapshot : never;
  serviceSnapshot: WindowsServiceSnapshotSet;
  priorServiceDefinitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
  nextServiceDefinitions: WindowsServiceDefinition[];
  runningBefore: Record<RoostServiceRole, boolean>;
  desiredRunning: SchemaVersion extends 2 ? Record<RoostServiceRole, boolean> : never;
  healthBefore: Partial<Record<"worker" | "coordinator", HealthCheckpoint>>;
  stoppedRoles: RoostServiceRole[];
  restoredRoles: RoostServiceRole[];
  retainedVersionDirs: string[];
  progress: WindowsUpdateProgressEntry[];
  failure?: { forwardPhase: WindowsUpdateForwardPhase; error: string; at: string };
  rollbackFailure?: { phase: WindowsUpdateRollbackPhase; error: string; at: string };
}

/** Typed representation of update-v1.json. Build fields were absent in early writers. */
export type WindowsUpdateJournalV1 = Omit<WindowsUpdateJournalFields<
  1,
  undefined,
  WindowsCurrentManifestV1,
  WindowsServiceHealthCheckpointV1
>, "targetBuild" | "stableArtifacts" | "desiredRunning" | "currentManifestSnapshot"> & {
  targetBuild?: string;
};

export type WindowsUpdateJournalV2 = WindowsUpdateJournalFields<
  2,
  string,
  WindowsCurrentManifestV2,
  WindowsServiceHealthCheckpointV2
>;

export type WindowsUpdateJournal = WindowsUpdateJournalV1 | WindowsUpdateJournalV2;

export interface CreateWindowsUpdateJournalInput {
  jobId?: string;
  targetVersion: string;
  targetBuild: string;
  signedManifest: WindowsUpdateJournalV2["signedManifest"];
  releasePackage: WindowsUpdateJournalV2["releasePackage"];
  assets: WindowsReleaseFile[];
  stableArtifactMode?: WindowsStableArtifacts["mode"];
  paths: WindowsUpdateJournalV2["paths"];
  currentManifest: WindowsUpdateJournalV2["currentManifest"];
  currentManifestSnapshot: WindowsCurrentManifestSnapshot;
  serviceSnapshot: WindowsServiceSnapshotSet;
  priorServiceDefinitions: WindowsUpdateJournalV2["priorServiceDefinitions"];
  nextServiceDefinitions: WindowsServiceDefinition[];
  runningBefore: Record<RoostServiceRole, boolean>;
  desiredRunning?: Record<RoostServiceRole, boolean>;
  healthBefore: WindowsUpdateJournalV2["healthBefore"];
  now?: () => Date;
  transactionId?: string;
}

export interface WindowsUpdateJournalStore {
  readonly path: string;
  readonly legacyPath?: string;
  load(): Promise<WindowsUpdateJournal | null>;
  save(journal: WindowsUpdateJournalV2): Promise<void>;
  migrateLegacy?(journal: WindowsUpdateJournalV2): Promise<void>;
}

export function windowsUpdateJournalPath(serviceDir: string = roostServiceDir()): string {
  return join(serviceDir, WINDOWS_UPDATE_JOURNAL_FILE);
}

export function windowsLegacyUpdateJournalPath(serviceDir: string = roostServiceDir()): string {
  return join(serviceDir, WINDOWS_LEGACY_UPDATE_JOURNAL_FILE);
}

export function windowsCurrentManifestPath(serviceDir: string = roostServiceDir()): string {
  return join(serviceDir, WINDOWS_CURRENT_MANIFEST_FILE);
}

export function sha256Hex(bytes: string | Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}
export function createWindowsStableArtifacts(
  transactionId: string,
  assets: readonly WindowsReleaseFile[],
  paths: Pick<WindowsUpdateJournalV2["paths"], "newVersionDir" | "currentManifestPath">,
  mode: WindowsStableArtifacts["mode"] = "promote",
): WindowsStableArtifacts {
  const serviceDir = dirname(paths.currentManifestPath);
  const stableBin = join(dirname(serviceDir), "bin");
  const backupDir = join(
    serviceDir,
    "data",
    "updater",
    "updates",
    sha256Hex(nonempty(transactionId, "journal.transactionId")),
    "stable-artifacts",
  );
  const plan = (
    releaseName: "shawl.exe" | "roost-win-helper.exe",
    stableName: "shawl.exe" | "roost.exe",
  ): WindowsStableArtifactPlan => {
    const asset = assets.find(({ path }) => path.toLowerCase() === releaseName);
    if (!asset || !asset.authenticodeRequired) {
      throw new Error(`Windows release requires an Authenticode-verified ${releaseName}`);
    }
    return {
      releasePath: join(paths.newVersionDir, asset.path),
      stablePath: join(stableBin, stableName),
      sha256: asset.sha256,
      size: asset.size,
      prior: null,
    };
  };
  return {
    mode,
    backupDir,
    shawl: plan("shawl.exe", "shawl.exe"),
    launcher: plan("roost-win-helper.exe", "roost.exe"),
  };
}
export function desiredWindowsServiceLifecycle(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
): Record<RoostServiceRole, boolean> {
  return {
    keeper: definitions.keeper.startMode === "automatic",
    worker: definitions.worker.startMode === "automatic",
    coordinator: definitions.coordinator.startMode === "automatic",
    updater: false,
  };
}


export function parseWindowsBuildIdentity(value: unknown, label = "build"): string {
  return buildIdentity(value, label);
}

export function parseWindowsReleaseManifest(raw: string | Uint8Array): WindowsReleaseManifestV1 {
  const o = record(
    parseJson(typeof raw === "string" ? raw : Buffer.from(raw).toString("utf8"), "Windows release manifest"),
    "Windows release manifest",
  );
  if (o.schemaVersion !== 1) {
    throw new Error(`unsupported Windows release manifest schema: ${String(o.schemaVersion)}`);
  }
  if (o.platform !== "win32" || o.arch !== "x64") {
    throw new Error(`manifest targets ${String(o.platform)}/${String(o.arch)}, expected win32/x64`);
  }
  const version = releaseVersion(o.version, "manifest.version");
  const build = buildIdentity(o.build, "manifest.build");
  const publishedAt = isoTimestamp(o.publishedAt, "manifest.publishedAt");
  const pkg = record(o.package, "manifest.package");
  if (pkg.name !== "roost-windows-x64.zip") throw new Error("unexpected Windows package name");
  if (!Array.isArray(o.files) || o.files.length === 0) throw new Error("manifest.files must be non-empty");
  const seen = new Set<string>();
  const files = o.files.map((value, index): WindowsReleaseFile => {
    const f = record(value, `manifest.files[${index}]`);
    const path = safeRelative(f.path, `manifest.files[${index}].path`);
    const folded = path.toLowerCase();
    if (seen.has(folded)) throw new Error(`duplicate manifest asset: ${path}`);
    seen.add(folded);
    if (typeof f.authenticodeRequired !== "boolean") {
      throw new Error(`manifest.files[${index}].authenticodeRequired is invalid`);
    }
    return {
      path,
      sha256: sha(f.sha256, `manifest.files[${index}].sha256`),
      size: fileSize(f.size, `manifest.files[${index}].size`),
      authenticodeRequired: f.authenticodeRequired,
    };
  });
  for (const required of ["roost.exe", "roost-win-helper.exe", "shawl.exe"]) {
    const asset = files.find(({ path }) => path.toLowerCase() === required);
    if (!asset) throw new Error(`manifest is missing ${required}`);
    if (!asset.authenticodeRequired) throw new Error(`manifest must require Authenticode for ${required}`);
  }
  const shawl = record(o.shawl, "manifest.shawl");
  if (shawl.version !== "1.9.0") {
    throw new Error(`unsupported Shawl version: ${String(shawl.version)}`);
  }
  // The upstream digest pins the Shawl release archive; it is not the digest
  // of the extracted, separately Authenticode-verified shawl.exe.
  const upstreamSha256 = sha(shawl.upstreamSha256, "manifest.shawl.upstreamSha256");
  return {
    schemaVersion: 1,
    version,
    build,
    platform: "win32",
    arch: "x64",
    publishedAt,
    package: {
      name: "roost-windows-x64.zip",
      sha256: sha(pkg.sha256, "manifest.package.sha256"),
      size: fileSize(pkg.size, "manifest.package.size"),
    },
    files,
    shawl: { version: "1.9.0", upstreamSha256 },
  };
}

export function parseWindowsCurrentManifest(raw: string): WindowsCurrentManifest {
  const o = record(parseJson(raw, "Windows current manifest"), "Windows current manifest");
  if (o.schemaVersion !== 1 && o.schemaVersion !== WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION) {
    throw new Error(`unsupported Windows current manifest schema: ${String(o.schemaVersion)}`);
  }
  if (!Array.isArray(o.files)) throw new Error("current manifest files must be an array");
  const files = parseCurrentFiles(o.files);
  const fields: WindowsCurrentManifestFields = {
    version: releaseVersion(o.version, "current version"),
    versionDir: absolutePath(o.versionDir, "current versionDir"),
    files,
    manifestUrl: httpsUrl(o.manifestUrl, "current manifestUrl"),
    manifestSha256: sha(o.manifestSha256, "current manifestSha256"),
    publisherSha256: normalizedSha(o.publisherSha256, "current publisherSha256"),
  };
  if (o.schemaVersion === 1) {
    const build = Object.hasOwn(o, "build")
      ? buildIdentity(o.build, "legacy current build")
      : undefined;
    return { schemaVersion: 1, ...fields, ...(build ? { build } : {}) };
  }
  return {
    schemaVersion: WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION,
    ...fields,
    build: buildIdentity(o.build, "current build"),
  };
}

export function migrateWindowsCurrentManifestV1(
  current: WindowsCurrentManifestV1,
  authenticatedBuild: string,
): WindowsCurrentManifestV2 {
  const build = buildIdentity(authenticatedBuild, "authenticated current build");
  if (current.build && current.build !== build) {
    throw new Error("legacy current manifest build disagrees with authenticated running build");
  }
  const migrated: WindowsCurrentManifestV2 = {
    ...current,
    schemaVersion: WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION,
    build,
  };
  const parsed = parseWindowsCurrentManifest(JSON.stringify(migrated));
  if (parsed.schemaVersion !== 2) throw new Error("current manifest migration did not produce schema 2");
  return parsed;
}

export function createWindowsUpdateJournal(input: CreateWindowsUpdateJournalInput): WindowsUpdateJournalV2 {
  const now = (input.now ?? (() => new Date()))().toISOString();
  const transactionId = input.transactionId ?? randomUUID();
  const journal: WindowsUpdateJournalV2 = {
    schemaVersion: WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION,
    transactionId,
    jobId: input.jobId ?? transactionId,
    createdAt: now,
    updatedAt: now,
    revision: 1,
    state: "forward",
    phase: "prepared",
    targetVersion: input.targetVersion,
    targetBuild: input.targetBuild,
    signedManifest: input.signedManifest,
    releasePackage: input.releasePackage,
    assets: input.assets,
    stableArtifacts: createWindowsStableArtifacts(
      transactionId,
      input.assets,
      input.paths,
      input.stableArtifactMode,
    ),
    paths: input.paths,
    currentManifest: input.currentManifest,
    currentManifestSnapshot: input.currentManifestSnapshot,
    serviceSnapshot: input.serviceSnapshot,
    priorServiceDefinitions: input.priorServiceDefinitions,
    nextServiceDefinitions: input.nextServiceDefinitions,
    runningBefore: input.runningBefore,
    healthBefore: input.healthBefore,
    desiredRunning: input.desiredRunning
      ?? desiredWindowsServiceLifecycle(input.priorServiceDefinitions),
    stoppedRoles: [],
    restoredRoles: [],
    retainedVersionDirs: input.paths.priorVersionDir ? [input.paths.priorVersionDir] : [],
    progress: [{
      sequence: 1,
      at: now,
      phase: "prepared",
      message: "update journal durably prepared before service mutation",
      terminal: false,
      success: false,
    }],
  };
  assertWindowsUpdateJournal(journal);
  return journal;
}

export function appendWindowsUpdateProgress(
  journal: WindowsUpdateJournalV2,
  phase: WindowsUpdatePhase,
  message: string,
  options: {
    state?: WindowsUpdateState;
    terminal?: boolean;
    success?: boolean;
    error?: string;
    now?: Date;
  } = {},
): WindowsUpdateJournalV2 {
  const at = (options.now ?? new Date()).toISOString();
  const entry: WindowsUpdateProgressEntry = {
    sequence: (journal.progress.at(-1)?.sequence ?? 0) + 1,
    at,
    phase,
    message: bounded(message),
    terminal: options.terminal ?? false,
    success: options.success ?? false,
    ...(options.error ? { error: bounded(options.error) } : {}),
  };
  const next: WindowsUpdateJournalV2 = {
    ...journal,
    phase,
    state: options.state ?? journal.state,
    updatedAt: at,
    revision: journal.revision + 1,
    progress: [...journal.progress, entry].slice(-MAX_WINDOWS_UPDATE_PROGRESS),
  };
  assertWindowsUpdateJournal(next);
  return next;
}

export function readWindowsUpdateProgressFromJournal(
  journal: WindowsUpdateJournal | null,
  afterSequence = 0,
): WindowsUpdateProgressEntry[] {
  if (!Number.isSafeInteger(afterSequence) || afterSequence < 0) {
    throw new Error("afterSequence must be non-negative");
  }
  return journal ? journal.progress.filter((entry) => entry.sequence > afterSequence) : [];
}

export function parseWindowsUpdateJournal(raw: string): WindowsUpdateJournal {
  const value = parseJson(raw, "Windows update journal");
  const journal = record(value, "Windows update journal");
  if (journal.schemaVersion === 1) {
    assertLegacyWindowsUpdateJournal(value);
    return value;
  }
  const migrated = migrateMissingRecoveryPlan(value);
  assertWindowsUpdateJournal(migrated);
  return migrated;
}

function migrateMissingRecoveryPlan(value: unknown): unknown {
  const journal = record(value, "Windows update journal");
  const missingStable = !Object.hasOwn(journal, "stableArtifacts");
  const missingLifecycle = !Object.hasOwn(journal, "desiredRunning");
  const missingCurrentSnapshot = !Object.hasOwn(journal, "currentManifestSnapshot");
  if (missingCurrentSnapshot) {
    throw new Error(
      "Windows update journal lacks exact prior current-manifest security recovery data; "
        + "signed elevated installer migration is required before mutation",
    );
  }
  if (!missingStable && !missingLifecycle && !missingCurrentSnapshot) return value;
  if (
    journal.schemaVersion !== WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION
    || journal.state !== "forward"
    || ![
      "prepared",
      "broker-started",
      "assets-staged",
      "stable-artifacts-snapshotted",
    ].includes(String(journal.phase))
    || !Array.isArray(journal.assets)
  ) {
    throw new Error(
      "schema 2 Windows update journal predates complete recovery data after the mutation boundary",
    );
  }
  const paths = record(journal.paths, "journal.paths");
  const definitions = record(
    journal.priorServiceDefinitions,
    "journal.priorServiceDefinitions",
  ) as unknown as Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
  return {
    ...journal,
    stableArtifacts: missingStable
      ? createWindowsStableArtifacts(
        nonempty(journal.transactionId, "journal.transactionId"),
        journal.assets as WindowsReleaseFile[],
        {
          newVersionDir: nonempty(paths.newVersionDir, "journal.paths.newVersionDir"),
          currentManifestPath: nonempty(paths.currentManifestPath, "journal.paths.currentManifestPath"),
        },
      )
      : journal.stableArtifacts,
    desiredRunning: missingLifecycle
      ? desiredWindowsServiceLifecycle(definitions)
      : journal.desiredRunning,
    currentManifestSnapshot: journal.currentManifestSnapshot,
  };
}

export function assertWindowsUpdateJournal(value: unknown): asserts value is WindowsUpdateJournalV2 {
  assertJournalCommon(value, WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION, true);
}

export function assertLegacyWindowsUpdateJournal(value: unknown): asserts value is WindowsUpdateJournalV1 {
  assertJournalCommon(value, 1, false);
}

function assertJournalCommon(value: unknown, schemaVersion: 1 | 2, requireBuilds: boolean): void {
  const j = record(value, "Windows update journal");
  if (j.schemaVersion !== schemaVersion) {
    throw new Error(`unsupported Windows update journal schema: ${String(j.schemaVersion)}`);
  }
  nonempty(j.transactionId, "journal.transactionId");
  nonempty(j.jobId, "journal.jobId");
  isoTimestamp(j.createdAt, "journal.createdAt");
  isoTimestamp(j.updatedAt, "journal.updatedAt");
  releaseVersion(j.targetVersion, "journal.targetVersion");
  if (requireBuilds || Object.hasOwn(j, "targetBuild")) {
    buildIdentity(j.targetBuild, "journal.targetBuild");
  }
  if (!Number.isSafeInteger(j.revision) || (j.revision as number) < 1) {
    throw new Error("journal.revision is invalid");
  }
  if (!isState(j.state) || !isPhase(j.phase)) throw new Error("journal state/phase is invalid");
  if (requireBuilds && j.phase === "service-configs-switched") {
    throw new Error("schema 2 journal cannot contain the retired service-config switch phase");
  }
  if (
    !requireBuilds
    && (
      j.phase === "cleanup-complete"
      || j.phase === "stable-artifacts-promoted"
      || j.phase === "rollback-stable-artifacts-restored"
    )
  ) {
    throw new Error("schema 1 journal cannot contain schema 2 stable-artifact phases");
  }

  const signed = record(j.signedManifest, "journal.signedManifest");
  httpsUrl(signed.url, "manifest URL");
  httpsUrl(signed.signatureUrl, "manifest signature URL");
  const proofOnly = requireBuilds
    && record(j.stableArtifacts, "journal.stableArtifacts").mode === "proof-only";
  if (proofOnly) {
    if (signed.path !== null || signed.signaturePath !== null || j.releasePackage !== null) {
      throw new Error("proof-only journal cannot contain staged signed or package artifacts");
    }
  } else {
    absolutePath(signed.path, "manifest path");
    absolutePath(signed.signaturePath, "manifest signature path");
    const pkg = record(j.releasePackage, "journal.releasePackage");
    httpsUrl(pkg.url, "package URL");
    absolutePath(pkg.path, "package path");
    sha(pkg.sha256, "package digest");
    fileSize(pkg.size, "package size");
  }
  sha(signed.sha256, "manifest digest");
  sha(signed.publisherSha256, "publisher digest");

  if (!Array.isArray(j.assets) || j.assets.length === 0) throw new Error("journal assets are missing");
  const assetPaths = new Set<string>();
  for (const [index, value] of j.assets.entries()) {
    const asset = record(value, `journal.assets[${index}]`);
    const path = safeRelative(asset.path, `journal.assets[${index}].path`);
    if (assetPaths.has(path.toLowerCase())) throw new Error(`duplicate journal asset: ${path}`);
    assetPaths.add(path.toLowerCase());
    sha(asset.sha256, `journal.assets[${index}].sha256`);
    fileSize(asset.size, `journal.assets[${index}].size`);
    if (typeof asset.authenticodeRequired !== "boolean") throw new Error("invalid Authenticode flag");
  }

  const paths = record(j.paths, "journal.paths");
  absolutePath(paths.newVersionDir, "journal.paths.newVersionDir");
  absolutePath(paths.stagingDir, "journal.paths.stagingDir");
  absolutePath(paths.currentManifestPath, "journal.paths.currentManifestPath");
  if (paths.priorVersionDir !== null) {
    absolutePath(paths.priorVersionDir, "journal.paths.priorVersionDir");
  }
  if (requireBuilds) validateStableArtifacts(j.stableArtifacts);

  const current = record(j.currentManifest, "journal.currentManifest");
  if (current.priorRaw !== null && typeof current.priorRaw !== "string") {
    throw new Error("journal prior current manifest is invalid");
  }
  if (typeof current.priorRaw === "string") {
    const prior = parseWindowsCurrentManifest(current.priorRaw);
    if (prior.schemaVersion !== schemaVersion) {
      throw new Error(`journal schema ${schemaVersion} requires a schema ${schemaVersion} prior current manifest`);
    }
  }
  const next = parseWindowsCurrentManifest(JSON.stringify(current.next));
  if (next.schemaVersion !== schemaVersion) {
    throw new Error(`journal schema ${schemaVersion} requires a schema ${schemaVersion} next current manifest`);
  }
  if (requireBuilds) validateCurrentManifestSnapshot(j.currentManifestSnapshot, current.priorRaw);

  const snapshots = record(j.serviceSnapshot, "journal.serviceSnapshot");
  for (const role of SERVICE_ROLES) {
    if (!snapshots[role] || record(snapshots[role], `journal snapshot ${role}`).role !== role) {
      throw new Error(`journal snapshot missing ${role}`);
    }
  }
  const priorDefinitions = record(j.priorServiceDefinitions, "journal.priorServiceDefinitions");
  for (const role of SERVICE_ROLES) {
    if (record(priorDefinitions[role], `journal prior definition ${role}`).role !== role) {
      throw new Error(`journal prior definitions missing ${role}`);
    }
  }
  if (!Array.isArray(j.nextServiceDefinitions) || j.nextServiceDefinitions.length !== 4) {
    throw new Error("journal requires four next definitions");
  }
  const definitionRoles = new Set(
    j.nextServiceDefinitions.map((definition) => record(definition, "journal service definition").role),
  );
  for (const role of SERVICE_ROLES) {
    if (!definitionRoles.has(role)) throw new Error(`journal next definitions missing ${role}`);
  }
  if (requireBuilds) {
    const desired = record(j.desiredRunning, "journal.desiredRunning");
    for (const role of SERVICE_ROLES) {
      if (typeof desired[role] !== "boolean") {
        throw new Error(`invalid desired lifecycle ${role}`);
      }
    }
    if (desired.updater !== false) {
      throw new Error("updater desired lifecycle must be stopped after the transaction");
    }
  }

  const running = record(j.runningBefore, "journal.runningBefore");
  for (const role of SERVICE_ROLES) {
    if (typeof running[role] !== "boolean") throw new Error(`invalid running vector ${role}`);

  }
  if (running.updater) throw new Error("updater was already running");

  const healthBefore = record(j.healthBefore, "journal.healthBefore");
  const proofOnlyMayLackFailedHealth = requireBuilds
    && record(j.stableArtifacts, "journal.stableArtifacts").mode === "proof-only"
    && j.state !== "succeeded";
  for (const role of ACTIVE_ROLES) {
    if (!(running[role] as boolean) && healthBefore[role] === undefined) continue;
    if (proofOnlyMayLackFailedHealth && healthBefore[role] === undefined) continue;
    const health = record(healthBefore[role], `journal healthBefore ${role}`);
    releaseVersion(health.version, `journal healthBefore ${role}.version`);
    if (requireBuilds || Object.hasOwn(health, "build")) {
      buildIdentity(health.build, `journal healthBefore ${role}.build`);
    }
    nonempty(health.processEpoch, `journal healthBefore ${role}.processEpoch`);
    if (role === "worker") httpsUrl(health.coordinatorUrl, "journal worker coordinatorUrl");
  }

  roleArray(j.stoppedRoles, "journal.stoppedRoles");
  roleArray(j.restoredRoles, "journal.restoredRoles");
  if (!Array.isArray(j.retainedVersionDirs)) throw new Error("journal.retainedVersionDirs is invalid");
  for (const [index, retained] of j.retainedVersionDirs.entries()) {
    absolutePath(retained, `journal.retainedVersionDirs[${index}]`);
  }
  validateProgress(j.progress, requireBuilds);

  if (j.failure !== undefined) {
    const failure = record(j.failure, "journal.failure");
    if (!isForwardPhase(failure.forwardPhase)) throw new Error("journal.failure.forwardPhase is invalid");
    nonempty(failure.error, "journal.failure.error");
    isoTimestamp(failure.at, "journal.failure.at");
  }
  if (j.rollbackFailure !== undefined) {
    const failure = record(j.rollbackFailure, "journal.rollbackFailure");
    if (!isRollbackPhase(failure.phase)) throw new Error("journal.rollbackFailure.phase is invalid");

    nonempty(failure.error, "journal.rollbackFailure.error");
    isoTimestamp(failure.at, "journal.rollbackFailure.at");
  }
}
function validateStableArtifacts(value: unknown): void {
  const stable = record(value, "journal.stableArtifacts");
  if (stable.mode !== "promote" && stable.mode !== "proof-only") {
    throw new Error("journal stable artifact mode is invalid");
  }
  absolutePath(stable.backupDir, "journal.stableArtifacts.backupDir");
  for (const name of ["shawl", "launcher"] as const) {
    const plan = record(stable[name], `journal.stableArtifacts.${name}`);
    absolutePath(plan.releasePath, `journal.stableArtifacts.${name}.releasePath`);
    absolutePath(plan.stablePath, `journal.stableArtifacts.${name}.stablePath`);
    sha(plan.sha256, `journal.stableArtifacts.${name}.sha256`);
    fileSize(plan.size, `journal.stableArtifacts.${name}.size`);
    if (plan.prior === null) continue;
    const prior = record(plan.prior, `journal.stableArtifacts.${name}.prior`);
    if (prior.existed === false) continue;
    if (prior.existed !== true) {
      throw new Error(`journal.stableArtifacts.${name}.prior.existed is invalid`);
    }
    absolutePath(prior.backupPath, `journal.stableArtifacts.${name}.prior.backupPath`);
    sha(prior.sha256, `journal.stableArtifacts.${name}.prior.sha256`);
    fileSize(prior.size, `journal.stableArtifacts.${name}.prior.size`);
    const descriptor = nonempty(
      prior.securityDescriptor,
      `journal.stableArtifacts.${name}.prior.securityDescriptor`,
    );
    if (descriptor.length > 64 * 1024 || descriptor.includes("\0")) {
      throw new Error(`journal.stableArtifacts.${name}.prior.securityDescriptor is invalid`);
    }
  }
  if (
    stable.mode === "proof-only"
    && (record(stable.shawl, "journal.stableArtifacts.shawl").prior !== null
      || record(stable.launcher, "journal.stableArtifacts.launcher").prior !== null)
  ) {
    throw new Error("proof-only journal cannot contain stable artifact backups");
  }
}

export class DurableWindowsUpdateJournalStore implements WindowsUpdateJournalStore {
  readonly legacyPath: string;

  constructor(
    readonly path: string = windowsUpdateJournalPath(),
    legacyPath: string = join(dirname(path), WINDOWS_LEGACY_UPDATE_JOURNAL_FILE),
  ) {
    this.legacyPath = legacyPath;
  }

  async load(): Promise<WindowsUpdateJournal | null> {
    const currentRaw = await readOptional(this.path);
    if (currentRaw !== null) {
      const journal = parseWindowsUpdateJournal(currentRaw);
      if (journal.schemaVersion !== WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION) {
        throw new Error(`legacy Windows update journal must remain at ${this.legacyPath}`);
      }
      return journal;
    }
    const legacyRaw = await readOptional(this.legacyPath);
    if (legacyRaw === null) return null;
    const journal = parseWindowsUpdateJournal(legacyRaw);
    if (journal.schemaVersion !== 1) {
      throw new Error(`Windows update journal at ${this.legacyPath} must use legacy schema 1`);
    }
    return journal;
  }

  async save(journal: WindowsUpdateJournalV2): Promise<void> {
    assertWindowsUpdateJournal(journal);
    await replaceWindowsUpdaterArtifact(this.path, `${JSON.stringify(journal)}\n`, "control");
    const statusPath = windowsUpdateStatusPath(journal.jobId, dirname(this.path));
    const status = {
      schemaVersion: 1,
      jobId: journal.jobId,
      progress: journal.progress,
    } as const;
    await replaceWindowsUpdaterArtifact(statusPath, `${JSON.stringify(status)}\n`, "status");
  }

  async migrateLegacy(journal: WindowsUpdateJournalV2): Promise<void> {
    await this.save(journal);
    if (this.legacyPath === this.path) return;
    await durableRemove(this.legacyPath);
    await flushDurablePath(dirname(this.legacyPath));
  }
}
export function windowsUpdateStatusPath(
  jobId: string,
  updaterDir: string = join(roostServiceDir(), "data", "updater"),
): string {
  const key = createHash("sha256").update(jobId).digest("hex");
  return join(updaterDir, `${WINDOWS_UPDATE_STATUS_PREFIX}${key}.json`);
}


export type WindowsUpdaterPersistenceProfile = WindowsUpdaterArtifactProfile;

export async function replaceWindowsUpdaterArtifact(
  path: string,
  contents: string | Uint8Array,
  profile: WindowsUpdaterPersistenceProfile,
): Promise<void> {
  await mkdir(dirname(path), { recursive: true });
  if (process.platform !== "win32") {
    await durableWriteFile(path, contents, { privateDacl: true });
    return;
  }
  const bytes = typeof contents === "string" ? Buffer.from(contents, "utf8") : contents;
  await windowsReplaceUpdaterArtifact(path, profile, bytes);
}

async function readOptional(path: string): Promise<string | null> {
  try {
    await lstat(path);
  } catch (error) {
    if (nodeError(error)?.code === "ENOENT") return null;
    throw error;
  }
  if (process.platform === "win32") {
    const bytes = await windowsReadUpdaterArtifact(path, "control", 16 * 1024 * 1024);
    return Buffer.from(bytes).toString("utf8");
  }
  return await readFile(path, "utf8");
}

function parseCurrentFiles(values: unknown[]): Array<Pick<WindowsReleaseFile, "path" | "sha256" | "size">> {
  const seen = new Set<string>();
  return values.map((value, index) => {
    const file = record(value, `current files[${index}]`);
    const path = safeRelative(file.path, `current files[${index}].path`);
    if (seen.has(path.toLowerCase())) throw new Error(`duplicate current manifest asset: ${path}`);
    seen.add(path.toLowerCase());
    return {
      path,
      sha256: sha(file.sha256, `current files[${index}].sha256`),
      size: fileSize(file.size, `current files[${index}].size`),
    };
  });
}

function validateProgress(value: unknown, requireBuilds: boolean): void {
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_WINDOWS_UPDATE_PROGRESS) {
    throw new Error("journal progress is invalid");
  }
  let sequence = 0;
  for (const entryValue of value) {
    const entry = record(entryValue, "journal progress");
    if (!Number.isSafeInteger(entry.sequence) || (entry.sequence as number) <= sequence) {
      throw new Error("journal progress ordering is invalid");
    }
    sequence = entry.sequence as number;
    isoTimestamp(entry.at, "progress.at");
    if (!isPhase(entry.phase)) throw new Error("journal progress phase is invalid");
    if (requireBuilds && entry.phase === "service-configs-switched") {
      throw new Error("schema 2 progress cannot contain the retired service-config switch phase");
    }
    if (
      !requireBuilds
      && (
        entry.phase === "cleanup-complete"
        || entry.phase === "stable-artifacts-promoted"
        || entry.phase === "rollback-stable-artifacts-restored"
      )
    ) {
      throw new Error("schema 1 progress cannot contain schema 2 stable-artifact phases");
    }
    const message = nonempty(entry.message, "progress message");
    if (message.length > 2048) throw new Error("progress message is too long");
    if (typeof entry.terminal !== "boolean" || typeof entry.success !== "boolean") {
      throw new Error("progress flags are invalid");
    }
    if (entry.error !== undefined) {
      const error = nonempty(entry.error, "progress error");
      if (error.length > 2048) throw new Error("progress error is too long");
    }
  }
}

function roleArray(value: unknown, label: string): void {
  if (!Array.isArray(value)) throw new Error(`${label} must be an array`);
  const seen = new Set<RoostServiceRole>();
  for (const role of value) {
    if (!SERVICE_ROLES.includes(role as RoostServiceRole) || seen.has(role as RoostServiceRole)) {
      throw new Error(`${label} contains an invalid or duplicate role`);
    }
    seen.add(role as RoostServiceRole);
  }
}

function parseJson(raw: string, label: string): unknown {
  try {
    return JSON.parse(raw);
  } catch (error) {
    throw new Error(`invalid ${label} JSON: ${String(error)}`);
  }
}

function record(value: unknown, label: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error(`${label} must be an object`);
  }
  return value as Record<string, unknown>;
}

function nonempty(value: unknown, label: string): string {
  if (typeof value !== "string" || value.length === 0 || value.includes("\0")) {
    throw new Error(`${label} must be non-empty`);
  }
  return value;
}

function sha(value: unknown, label: string): string {
  if (typeof value !== "string" || !SHA256_RE.test(value)) {
    throw new Error(`${label} must be lowercase SHA-256`);
  }
  return value;
}

function normalizedSha(value: unknown, label: string): string {
  const normalized = typeof value === "string" ? value.trim().toLowerCase() : "";
  if (!SHA256_RE.test(normalized)) throw new Error(`${label} must be SHA-256`);
  return normalized;
}

function buildIdentity(value: unknown, label: string): string {
  if (typeof value !== "string" || !BUILD_ID_RE.test(value)) {
    throw new Error(`${label} must be a lowercase immutable build id`);
  }
  return value;
}

function fileSize(value: unknown, label: string): number {
  if (!Number.isSafeInteger(value) || (value as number) < 0) {
    throw new Error(`${label} must be a non-negative safe integer`);
  }
  return value as number;
}

function isoTimestamp(value: unknown, label: string): string {
  const timestamp = nonempty(value, label);
  const parsed = Date.parse(timestamp);
  if (!Number.isFinite(parsed) || new Date(parsed).toISOString() !== timestamp) {
    throw new Error(`${label} must be an ISO-8601 UTC timestamp`);
  }
  return timestamp;
}

function httpsUrl(value: unknown, label: string): string {
  const text = nonempty(value, label);
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    throw new Error(`${label} must be absolute`);
  }
  if (url.protocol !== "https:") throw new Error(`${label} must use HTTPS`);
  return text;
}

function safeRelative(value: unknown, label: string): string {
  const path = nonempty(value, label).replaceAll("\\", "/");
  if (
    isAbsolute(path)
    || win32.isAbsolute(path)
    || posix.isAbsolute(path)
    || path.startsWith("/")
    || /^[A-Za-z]:/.test(path)
    || path.split("/").some((part) => !part || part === "." || part === "..")
  ) {
    throw new Error(`${label} is unsafe`);
  }
  return path;
}

function absolutePath(value: unknown, label: string): string {
  const path = nonempty(value, label);
  if (!isAbsolute(path) && !win32.isAbsolute(path) && !posix.isAbsolute(path)) {
    throw new Error(`${label} must be absolute`);
  }
  return path;
}

function releaseVersion(value: unknown, label: string): string {
  const version = nonempty(value, label);
  if (/[/\\]/.test(version) || version === "." || version === "..") {
    throw new Error(`${label} is unsafe`);
  }
  return version;
}

function bounded(value: string): string {
  return (value.replace(/[\r\n]+/g, " ").trim() || "update progress").slice(0, 2048);
}

function isState(value: unknown): value is WindowsUpdateState {
  return value === "forward"
    || value === "rolling-back"
    || value === "succeeded"
    || value === "rolled-back";
}

function isForwardPhase(value: unknown): value is WindowsUpdateLegacyForwardPhase {
  return [
    "prepared",
    "broker-started",
    "assets-staged",
    "stable-artifacts-snapshotted",
    "services-stopped",
    "stable-artifacts-promoted",
    "service-configs-switched",
    "updater-config-switched",
    "current-manifest-switched",
    "services-restored",
    "health-proven",
    "cleanup-complete",
    "committed",
  ].includes(String(value));
}

function isRollbackPhase(value: unknown): value is WindowsUpdateRollbackPhase {
  return [
    "rollback-started",
    "rollback-services-stopped",
    "rollback-stable-artifacts-restored",
    "rollback-configs-restored",
    "rollback-current-manifest-restored",
    "rollback-services-restored",
    "rolled-back",
  ].includes(String(value));
}

function isPhase(value: unknown): value is WindowsUpdatePhase {
  return isForwardPhase(value) || isRollbackPhase(value);
}

function nodeError(error: unknown): NodeJS.ErrnoException | null {
  return error instanceof Error && "code" in error ? error as NodeJS.ErrnoException : null;
}

export {
  DurableWindowsRelocationJournalStore,
  windowsRelocationJournalPath,
} from "./windows-relocation-journal.ts";
export type {
  WindowsRelocationJournalStore,
  WindowsRelocationJournalV1,
  WindowsRelocationPhase,
} from "./windows-relocation-journal.ts";

function validateCurrentManifestSnapshot(value: unknown, priorRaw: unknown): void {
  const snapshot = record(value, "journal.currentManifestSnapshot");
  sha(snapshot.sha256, "journal.currentManifestSnapshot.sha256");
  fileSize(snapshot.size, "journal.currentManifestSnapshot.size");
  const descriptor = nonempty(
    snapshot.securityDescriptor,
    "journal.currentManifestSnapshot.securityDescriptor",
  );
  if (descriptor.length > 64 * 1024 || descriptor.includes("\0")) {
    throw new Error("journal.currentManifestSnapshot.securityDescriptor is invalid");
  }
  if (typeof priorRaw !== "string") {
    throw new Error("schema 2 journal requires exact prior current manifest bytes");
  }
  const bytes = Buffer.from(priorRaw);
  if (snapshot.size !== bytes.byteLength || snapshot.sha256 !== sha256Hex(bytes)) {
    throw new Error("journal current manifest snapshot does not match prior bytes");
  }
}
