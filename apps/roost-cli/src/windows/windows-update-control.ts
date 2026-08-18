import { createHash, randomUUID } from "node:crypto";
import { once } from "node:events";
import { createWriteStream } from "node:fs";
import { mkdir, readFile, readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { finished } from "node:stream/promises";
import { durableRemove, durableReplace, flushDurablePath } from "@roost/shared/durability";
import { roostServiceDir, roostVersionsDir } from "@roost/shared/paths";
import { probeServiceHealth } from "@roost/shared/service-health";
import {
  windowsConsumeUpdaterRequest,
  windowsCreateUpdaterRequest,
  windowsReadUpdaterArtifact,
} from "@roost/shared/windows-helper";
import {
  WINDOWS_SERVICE_NAMES,
  WINDOWS_SERVICE_ROLES,
  createWindowsServiceManager,
  loadWindowsServiceDefinitions,
  retargetWindowsUpdaterDefinition,
} from "../service-ctl.ts";
import type {
  RoostServiceRole,
  WindowsServiceDefinition,
  WindowsServiceManager,
  WindowsServiceSnapshotSet,
} from "../service-ctl.ts";
import { assertStableWindowsUpdateTopology, type WindowsUpdateNative } from "./windows-update-broker.ts";
import { createWindowsUpdateNative } from "./windows-update-runtime.ts";
import {
  DurableWindowsUpdateJournalStore,
  createWindowsUpdateJournal,
  desiredWindowsServiceLifecycle,
  parseWindowsBuildIdentity,
  parseWindowsCurrentManifest,
  parseWindowsReleaseManifest,
  readWindowsUpdateProgressFromJournal,
  replaceWindowsUpdaterArtifact,
  sha256Hex,
  windowsUpdateStatusPath,
  windowsCurrentManifestPath,
} from "./windows-update-journal.ts";
import type {
  WindowsCurrentManifestV2,
  WindowsUpdateJournalStore,
  WindowsUpdateJournalV2,
  WindowsUpdateProgressEntry,
} from "./windows-update-journal.ts";
import {
  DurableWindowsRelocationJournalStore,
  windowsRelocationJournalPath,
} from "./windows-relocation-journal.ts";

const SHA256_RE = /^[0-9a-f]{64}$/;
const MAX_PENDING_UPDATE_REQUESTS = 16;
const MAX_UPDATE_REQUEST_BYTES = 32 * 1024;

export interface WindowsUpdateBrokerCommand {
  requestId: string;
  jobId: string;
  action: "START" | "STATUS";
  manifestUrl: string;
  signatureUrl: string;
  manifestSha256: string;
  publisherSha256: string;
  afterSequence?: number;
}

export interface WindowsUpdateProgressFrame {
  requestId: string;
  jobId: string;
  sequence: number;
  phase: string;
  message: string;
  terminal: boolean;
  success: boolean;
  error: string;
}

interface WindowsUpdateAdmissionRecord {
  schemaVersion: 1;
  kind: "update";
  command: WindowsUpdateBrokerCommand;
}

export interface WindowsUpdateControlDeps {
  store: WindowsUpdateJournalStore;
  services: WindowsServiceManager;
  native: WindowsUpdateNative;
  fetch: typeof fetch;
  serviceDir: string;
  versionsDir: string;
  currentManifestPath: string;
  requestDir: string;
  admissionRequestDirs?: readonly string[];
  probeHealth: typeof probeServiceHealth;
  platform?: NodeJS.Platform;
  readStatus?: (
    jobId: string,
    afterSequence: number,
    requestId: string,
  ) => Promise<WindowsUpdateProgressFrame[]>;
  createRequest?: typeof windowsCreateUpdaterRequest;
}
export function windowsUpdateRequestDirectory(
  serviceDir: string,
  serviceRole = process.env.ROOST_SERVICE_ROLE,
): string {
  const serviceRequestDir = join(serviceDir, "requests");
  return serviceRole === "worker" || serviceRole === "coordinator"
    ? serviceRequestDir
    : join(serviceRequestDir, "interactive-update");
}



/** Unprivileged callers can only enqueue an untrusted request and start the updater. */
export async function handleUpdateBrokerCommand(
  command: WindowsUpdateBrokerCommand,
  deps: WindowsUpdateControlDeps = defaultDeps(),
): Promise<WindowsUpdateProgressFrame[]> {
  validateCommand(command);
  const platform = deps.platform ?? process.platform;
  if (command.action === "STATUS") {
    if (deps.readStatus) {
      return await deps.readStatus(
        command.jobId,
        command.afterSequence ?? 0,
        command.requestId,
      );
    }
    return await readWindowsUpdateProgress(
      command.jobId,
      command.afterSequence ?? 0,
      command.requestId,
      deps.store,
    );
  }
  if (platform !== "win32") throw new Error(`Windows update broker command refused on ${platform}`);
  const requestPath = updateRequestPath(deps.requestDir, command.jobId);
  const admission: WindowsUpdateAdmissionRecord = {
    schemaVersion: 1,
    kind: "update",
    command: { ...command, afterSequence: undefined },
  };
  const admissionBytes = Buffer.from(`${JSON.stringify(admission)}\n`);
  if (admissionBytes.byteLength > MAX_UPDATE_REQUEST_BYTES) {
    throw new Error("Windows update admission exceeds the bounded request limit");
  }
  await (deps.createRequest ?? windowsCreateUpdaterRequest)(requestPath, admissionBytes);
  if ((await deps.services.query("updater")).state !== "running") {
    await deps.services.start("updater");
  }
  return [{
    requestId: command.requestId,
    jobId: command.jobId,
    sequence: 0,
    phase: "admission-requested",
    message: "signed update admission queued for the constrained updater service",
    terminal: false,
    success: false,
    error: "",
  }];
}

/**
 * Compatibility preflight for the SCM entrypoint. Admission is deliberately
 * non-mutating here: runWindowsUpdateBroker owns the sole machine lease and
 * calls admitPendingWindowsUpdateRequestWithinTransaction while holding it.
 */
export async function admitPendingWindowsUpdateRequest(
  deps: WindowsUpdateControlDeps = defaultDeps(),
): Promise<WindowsUpdateJournalV2 | null> {
  const platform = deps.platform ?? process.platform;
  if (platform !== "win32") throw new Error(`Windows update admission refused on ${platform}`);
  await deps.native.assertUpdaterServiceContext();
  return null;
}

/** Must only be called by runWindowsUpdateBroker while its machine lease is held. */
export async function admitPendingWindowsUpdateRequestWithinTransaction(
  deps: WindowsUpdateControlDeps = defaultDeps(),
): Promise<WindowsUpdateJournalV2 | null> {
  const platform = deps.platform ?? process.platform;
  if (platform !== "win32") throw new Error(`Windows update admission refused on ${platform}`);
  await deps.native.assertUpdaterServiceContext();
  const admissionDirs = deps.admissionRequestDirs ?? [deps.requestDir];
  let pending: Awaited<ReturnType<typeof readPendingUpdateRequest>> = null;
  for (const requestDir of admissionDirs) {
    pending = await readPendingUpdateRequest(requestDir);
    if (pending) break;
  }
  if (!pending) return null;

  const existing = await deps.store.load();
  if (existing && existing.jobId === pending.record.command.jobId) {
    await consumeUpdateRequest(pending.path);
    return existing.schemaVersion === 2 ? existing : null;
  }
  if (existing && (existing.state === "forward" || existing.state === "rolling-back")) {
    throw new Error(`Windows update transaction ${existing.jobId} is already active`);
  }
  const journal = await prepareAdmittedUpdate(pending.record.command, deps);
  await consumeUpdateRequest(pending.path, deps.requestDir, journal.jobId);
  return journal;
}

async function prepareAdmittedUpdate(
  command: WindowsUpdateBrokerCommand,
  deps: WindowsUpdateControlDeps,
): Promise<WindowsUpdateJournalV2> {
  validateCommand(command);
  if (command.action !== "START") throw new Error("update admission request must use START");

  const installedDefinitions = await loadWindowsServiceDefinitions();
  const observedSnapshot = await deps.services.snapshot({ includeSecurity: true });
  assertInstalledStableTopology(installedDefinitions, observedSnapshot, deps.serviceDir, true);
  const snapshotSet: WindowsServiceSnapshotSet = {
    ...observedSnapshot,
    updater: { ...observedSnapshot.updater, state: "stopped" },
  };
  const runningBefore = Object.fromEntries(
    Object.values(snapshotSet).map((snapshot) => [snapshot.role, snapshot.state === "running"]),
  ) as Record<RoostServiceRole, boolean>;
  runningBefore.updater = false;
  const healthBefore: WindowsUpdateJournalV2["healthBefore"] = {};
  for (const role of ["worker", "coordinator"] as const) {
    if (!runningBefore[role]) continue;
    const health = await deps.probeHealth(role);
    if (health.role !== role || !health.version || !health.build || !health.processEpoch) {
      throw new Error(`${role} did not provide a complete pre-update health checkpoint`);
    }
    const checkpoint = {
      version: health.version,
      build: parseWindowsBuildIdentity(health.build, `${role} running build`),
      processEpoch: health.processEpoch,
    };
    if (health.role === "worker") {
      if (!health.coordinatorUrl) throw new Error("worker did not provide its pre-update coordinator URL");
      healthBefore.worker = { ...checkpoint, coordinatorUrl: health.coordinatorUrl };
    } else {
      healthBefore.coordinator = checkpoint;
    }
  }

  const currentBytes = deps.native.readArtifact
    ? await deps.native.readArtifact(deps.currentManifestPath, "current", 16 * 1024 * 1024)
    : (deps.platform ?? process.platform) === "win32"
      ? (() => { throw new Error("held Windows current-manifest read is unavailable"); })()
      : await readFile(deps.currentManifestPath);
  const currentRaw = Buffer.from(currentBytes).toString("utf8");
  let parsedCurrent = parseWindowsCurrentManifest(currentRaw);
  if (parsedCurrent.schemaVersion === 1) {
    throw new Error(
      "legacy Windows current manifest requires signed elevated installer migration before updating",
    );
  }
  const current: WindowsCurrentManifestV2 = parsedCurrent;
  const environmentPublisher = (process.env.ROOST_WINDOWS_PUBLISHER_SHA256 ?? "").trim().toLowerCase();
  if (environmentPublisher && environmentPublisher !== current.publisherSha256) {
    throw new Error("protected Windows publisher pins disagree");
  }
  const localPublisher = environmentPublisher || current.publisherSha256;
  const commandPublisher = command.publisherSha256.trim().toLowerCase();
  if (!SHA256_RE.test(localPublisher)) throw new Error("DACL-protected local Windows publisher pin is missing");
  if (commandPublisher && commandPublisher !== localPublisher) {
    throw new Error("remote publisher pin disagrees with protected local pin");
  }
  const currentExpected = {
    sha256: sha256Hex(currentBytes),
    size: currentBytes.byteLength,
  };
  const currentInspection = deps.native.inspectArtifact
    ? await deps.native.inspectArtifact(deps.currentManifestPath, "current", currentExpected)
    : (deps.platform ?? process.platform) === "win32"
      ? (() => { throw new Error("held Windows current-manifest security proof is unavailable"); })()
      : { ...currentExpected, sddl: "non-windows-test-security-descriptor" };
  const currentManifestSnapshot: WindowsUpdateJournalV2["currentManifestSnapshot"] = {
    ...currentExpected,
    securityDescriptor: currentInspection.sddl,
  };

  const stagingDir = join(deps.serviceDir, "updates", createHash("sha256").update(command.jobId).digest("hex"));
  const manifestPath = join(stagingDir, "roost-windows-x64.manifest.json");
  const signaturePath = `${manifestPath}.p7s`;
  if (current.manifestSha256.toLowerCase() === command.manifestSha256.toLowerCase()) {
    const assets = current.files.map((asset) => ({
      ...asset,
      authenticodeRequired: [
        "roost.exe",
        "roost-win-helper.exe",
        "shawl.exe",
      ].includes(asset.path.toLowerCase()),
    }));
    const proof = createWindowsUpdateJournal({
      jobId: command.jobId,
      targetVersion: current.version,
      targetBuild: current.build,
      stableArtifactMode: "proof-only",
      signedManifest: {
        url: current.manifestUrl,
        signatureUrl: command.signatureUrl,
        path: null,
        signaturePath: null,
        sha256: current.manifestSha256,
        publisherSha256: localPublisher,
      },
      releasePackage: null,
      assets,
      paths: {
        priorVersionDir: current.versionDir,
        newVersionDir: current.versionDir,
        stagingDir,
        currentManifestPath: deps.currentManifestPath,
      },
      currentManifest: { priorRaw: currentRaw, next: current },
      currentManifestSnapshot,
      serviceSnapshot: snapshotSet,
      priorServiceDefinitions: installedDefinitions,
      nextServiceDefinitions: Object.values(installedDefinitions),
      runningBefore,
      desiredRunning: desiredWindowsServiceLifecycle(installedDefinitions),
      healthBefore,
    });
    await deps.store.save(proof);
    return proof;
  }
  await mkdir(stagingDir, { recursive: true });
  await deps.native.protectArtifacts(stagingDir);
  await downloadDurable(command.manifestUrl, manifestPath, deps.fetch, deps.native);
  await downloadDurable(command.signatureUrl, signaturePath, deps.fetch, deps.native);
  const manifestRaw = await readFile(manifestPath);
  if (sha256Hex(manifestRaw) !== command.manifestSha256) {
    throw new Error("downloaded manifest digest does not match command");
  }
  await deps.native.verifyCmsDetached(manifestPath, signaturePath, localPublisher);
  const manifest = parseWindowsReleaseManifest(manifestRaw);
  const releaseIdentity = compareWindowsReleaseIdentity(current, manifest);
  const sameRelease = releaseIdentity === "same"
    && current.manifestSha256.toLowerCase() === command.manifestSha256.toLowerCase();
  if (!sameRelease && releaseIdentity !== "different") {
    throw new Error(`release version ${manifest.version} collides with different immutable release metadata`);
  }

  const packageUrl = new URL(`./${manifest.package.name}`, command.manifestUrl).toString();
  const packagePath = join(stagingDir, manifest.package.name);
  await downloadDurable(packageUrl, packagePath, deps.fetch, deps.native, manifest.package.sha256, manifest.package.size);
  const newVersionDir = join(deps.versionsDir, manifest.version);
  if (!isUnder(deps.versionsDir, newVersionDir)) throw new Error("versioned binary path escaped versions directory");

  const next: WindowsCurrentManifestV2 = {
    schemaVersion: 2,
    version: manifest.version,
    build: manifest.build,
    versionDir: newVersionDir,
    files: manifest.files.map(({ path, sha256, size }) => ({ path, sha256, size })),
    manifestUrl: command.manifestUrl,
    manifestSha256: command.manifestSha256,
    publisherSha256: localPublisher,
  };
  const nextServiceDefinitions = {
    ...installedDefinitions,
    updater: retargetWindowsUpdaterDefinition(
      installedDefinitions.updater,
      newVersionDir,
    ),
  };
  const journal = createWindowsUpdateJournal({
    jobId: command.jobId,
    targetVersion: manifest.version,
    targetBuild: manifest.build,
    stableArtifactMode: sameRelease ? "proof-only" : "promote",
    signedManifest: {
      url: command.manifestUrl,
      signatureUrl: command.signatureUrl,
      path: manifestPath,
      signaturePath,
      sha256: command.manifestSha256,
      publisherSha256: localPublisher,
    },
    releasePackage: {
      url: packageUrl,
      path: packagePath,
      sha256: manifest.package.sha256,
      size: manifest.package.size,
    },
    assets: manifest.files,
    paths: { priorVersionDir: current.versionDir, newVersionDir, stagingDir, currentManifestPath: deps.currentManifestPath },
    currentManifestSnapshot,
    currentManifest: { priorRaw: currentRaw, next },
    serviceSnapshot: snapshotSet,
    priorServiceDefinitions: installedDefinitions,
    nextServiceDefinitions: Object.values(nextServiceDefinitions),
    runningBefore,
    desiredRunning: desiredWindowsServiceLifecycle(installedDefinitions),
    healthBefore,
  });
  await deps.store.save(journal);
  return journal;
}

export async function readWindowsUpdateProgress(
  jobId: string,
  afterSequence = 0,
  requestId = "status",
  store: WindowsUpdateJournalStore = new DurableWindowsUpdateJournalStore(),
): Promise<WindowsUpdateProgressFrame[]> {
  if (!jobId) throw new Error("jobId is required");
  const journal = await store.load();
  return !journal || journal.jobId !== jobId
    ? []
    : frames(requestId, jobId, readWindowsUpdateProgressFromJournal(journal, afterSequence));
}
export async function readPublishedWindowsUpdateProgress(
  jobId: string,
  afterSequence = 0,
  requestId = "status",
  serviceDir = roostServiceDir(),
  readArtifact: typeof windowsReadUpdaterArtifact = windowsReadUpdaterArtifact,
): Promise<WindowsUpdateProgressFrame[]> {
  if (!jobId) throw new Error("jobId is required");
  if (!Number.isSafeInteger(afterSequence) || afterSequence < 0) {
    throw new Error("afterSequence must be non-negative");
  }
  const path = windowsUpdateStatusPath(jobId, join(serviceDir, "data", "updater"));
  const bytes = await readArtifact(path, "status", 1024 * 1024);
  if (bytes.byteLength === 0) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(Buffer.from(bytes).toString("utf8"));
  } catch {
    throw new Error("Windows update status is not valid JSON");
  }
  if (!parsed || typeof parsed !== "object") throw new Error("Windows update status is invalid");
  const value = parsed as { schemaVersion?: unknown; jobId?: unknown; progress?: unknown };
  if (value.schemaVersion !== 1 || value.jobId !== jobId || !Array.isArray(value.progress)) {
    throw new Error("Windows update status has an unsupported schema");
  }
  if (value.progress.length > 128) throw new Error("Windows update status has too many entries");
  const entries = value.progress.map((entry, index): WindowsUpdateProgressEntry => {
    if (!entry || typeof entry !== "object") {
      throw new Error(`Windows update status entry ${index} is invalid`);
    }
    const item = entry as Record<string, unknown>;
    if (
      !Number.isSafeInteger(item.sequence)
      || (item.sequence as number) <= 0
      || typeof item.at !== "string"
      || typeof item.phase !== "string"
      || typeof item.message !== "string"
      || typeof item.terminal !== "boolean"
      || typeof item.success !== "boolean"
      || (item.error !== undefined && typeof item.error !== "string")
    ) {
      throw new Error(`Windows update status entry ${index} is malformed`);
    }
    return item as unknown as WindowsUpdateProgressEntry;
  });
  return frames(
    requestId,
    jobId,
    entries.filter((entry) => entry.sequence > afterSequence),
  );
}


function assertInstalledStableTopology(
  definitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>,
  snapshots: WindowsServiceSnapshotSet,
  serviceDir: string,
  requireSecurity = false,
): void {
  try {
    assertStableWindowsUpdateTopology(definitions, Object.values(definitions), serviceDir);
    for (const role of WINDOWS_SERVICE_ROLES) {
      const definition = definitions[role];
      const snapshot = snapshots[role];
      const expectedDependencies = definition.dependencies.map((dependency) => WINDOWS_SERVICE_NAMES[dependency].toLowerCase());
      const actualDependencies = snapshot.dependencies.map((dependency) => dependency.toLowerCase());
      if (!snapshot.installed
        || snapshot.imagePath !== definition.imagePath
        || snapshot.account?.trim().toLowerCase() !== definition.account.trim().toLowerCase()
        || snapshot.startMode !== definition.startMode
        || snapshot.displayName !== definition.displayName
        || snapshot.description !== definition.description
        || snapshot.serviceSidType !== "unrestricted"
        || JSON.stringify(Object.entries(snapshot.environment ?? {}).sort())
          !== JSON.stringify(Object.entries(definition.environment).sort())
        || (
          requireSecurity
          && (
            typeof snapshot.securityDescriptor !== "string"
            || snapshot.securityDescriptor.trim() === ""
          )
        )
        || JSON.stringify([...actualDependencies].sort()) !== JSON.stringify([...expectedDependencies].sort())) {
        throw new Error(`${snapshot.name} SCM state differs from its protected stable definition`);
      }
    }
  } catch (error) {
    throw new Error(
      `legacy or modified Windows SCM topology is not updateable in place: ${String(error)}; `
        + "rerun the signed elevated Roost installer to migrate",
    );
  }
}

async function readPendingUpdateRequest(
  requestDir: string,
): Promise<{ path: string; record: WindowsUpdateAdmissionRecord } | null> {
  let entries;
  try {
    entries = await readdir(requestDir, { withFileTypes: true });
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return null;
    throw error;
  }
  const names = entries
    .filter((entry) => entry.isFile() && /^update-[0-9a-f]{64}\.json$/.test(entry.name))
    .map((entry) => entry.name)
    .sort();
  if (names.length > MAX_PENDING_UPDATE_REQUESTS) {
    throw new Error("too many pending Windows update admissions");
  }
  const name = names[0];
  if (!name) return null;
  const path = join(requestDir, name);
  const bytes = await windowsReadUpdaterArtifact(path, "private", MAX_UPDATE_REQUEST_BYTES);
  let parsed: unknown;
  try {
    parsed = JSON.parse(Buffer.from(bytes).toString("utf8"));
  } catch {
    throw new Error("Windows update admission is not valid JSON");
  }
  if (!parsed || typeof parsed !== "object") throw new Error("Windows update admission is invalid");
  const value = parsed as Partial<WindowsUpdateAdmissionRecord>;
  if (value.schemaVersion !== 1 || value.kind !== "update" || !value.command) {
    throw new Error("Windows update admission has an unsupported schema");
  }
  validateCommand(value.command);
  return { path, record: value as WindowsUpdateAdmissionRecord };
}

function updateRequestPath(requestDir: string, jobId: string): string {
  return join(requestDir, `update-${createHash("sha256").update(jobId).digest("hex")}.json`);
}


async function consumeUpdateRequest(
  pendingPath: string,
  _requestDir?: string,
  _jobId?: string,
): Promise<void> {
  await windowsConsumeUpdaterRequest(pendingPath);
}

async function downloadDurable(
  url: string,
  destination: string,
  fetchImpl: typeof fetch,
  native: WindowsUpdateNative,
  expectedSha256?: string,
  expectedSize?: number,
): Promise<void> {
  requireHttps(url, "download URL");
  const response = await fetchImpl(url, { signal: AbortSignal.timeout(120_000) });
  if (!response.ok || !response.body) throw new Error(`download failed for ${url}: HTTP ${response.status}`);
  await mkdir(dirname(destination), { recursive: true });
  const temporary = `${destination}.${process.pid}.${randomUUID()}.tmp`;
  const output = createWriteStream(temporary, { flags: "wx", mode: 0o600 });
  const hash = createHash("sha256");
  let bytes = 0;
  const reader = response.body.getReader();
  try {
    while (true) {
      const { done, value } = await reader.read();
      if (done) break;
      hash.update(value);
      bytes += value.byteLength;
      if (!output.write(value)) await once(output, "drain");
    }
    output.end();
    await finished(output);
    await flushDurablePath(temporary, { platform: "win32" });
    await native.protectArtifacts(temporary);
    if (expectedSize !== undefined && bytes !== expectedSize) throw new Error(`download size mismatch for ${url}`);
    if (expectedSha256 !== undefined && hash.digest("hex") !== expectedSha256) {
      throw new Error(`download digest mismatch for ${url}`);
    }
    await durableReplace(temporary, destination, { platform: "win32" });
    await native.protectArtifacts(destination);
    await flushDurablePath(dirname(destination), { platform: "win32" });
  } catch (error) {
    output.destroy();
    throw error;
  } finally {
    reader.releaseLock();
    await durableRemove(temporary, { platform: "win32" }).catch(() => undefined);
  }
}

function frames(requestId: string, jobId: string, entries: readonly WindowsUpdateProgressEntry[]): WindowsUpdateProgressFrame[] {
  return entries.map((entry) => ({
    requestId,
    jobId,
    sequence: entry.sequence,
    phase: entry.phase,
    message: entry.message,
    terminal: entry.terminal,
    success: entry.success,
    error: entry.error ?? "",
  }));
}

function validateCommand(command: WindowsUpdateBrokerCommand): void {
  if (!command.requestId || !command.jobId || command.requestId.length > 256 || command.jobId.length > 256) {
    throw new Error("requestId/jobId required and must be bounded");
  }
  if (command.action !== "START" && command.action !== "STATUS") throw new Error("unknown update action");
  if (command.action !== "START") return;
  requireHttps(command.manifestUrl, "manifestUrl");
  requireHttps(command.signatureUrl, "signatureUrl");
  if (!SHA256_RE.test(command.manifestSha256)) throw new Error("invalid manifestSha256");
  if (command.publisherSha256 && !SHA256_RE.test(command.publisherSha256.trim().toLowerCase())) {
    throw new Error("invalid publisherSha256");
  }
}

function requireHttps(value: string, label: string): void {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    throw new Error(`${label} must be absolute`);
  }
  if (url.protocol !== "https:") throw new Error(`${label} must use HTTPS`);
}

function normalizedVersion(version: string): string {
  return version.replace(/^v/, "").split("+")[0]!;
}

export function compareWindowsReleaseIdentity(
  current: Readonly<{ version: string; build?: string }>,
  candidate: Readonly<{ version: string; build: string }>,
): "same" | "collision" | "different" {
  if (normalizedVersion(candidate.version) !== normalizedVersion(current.version)) return "different";
  return candidate.build === current.build ? "same" : "collision";
}

function isUnder(parent: string, child: string): boolean {
  const normalizedParent = parent.replaceAll("/", "\\").replace(/\\+$/, "").toLowerCase();
  const normalizedChild = child.replaceAll("/", "\\").toLowerCase();
  return normalizedChild.startsWith(`${normalizedParent}\\`);
}

function defaultDeps(): WindowsUpdateControlDeps {
  const serviceDir = roostServiceDir();
  const serviceRequestDir = join(serviceDir, "requests");
  const localRequestDir = join(serviceRequestDir, "interactive-update");
  const requestDir = windowsUpdateRequestDirectory(serviceDir);
  return {
    store: new DurableWindowsUpdateJournalStore(),
    services: createWindowsServiceManager(),
    native: createWindowsUpdateNative(),
    fetch,
    serviceDir,
    versionsDir: roostVersionsDir(),
    currentManifestPath: windowsCurrentManifestPath(serviceDir),
    requestDir,
    admissionRequestDirs: [serviceRequestDir, localRequestDir],
    probeHealth: probeServiceHealth,
    readStatus: async (jobId, afterSequence, requestId) =>
      await readPublishedWindowsUpdateProgress(jobId, afterSequence, requestId, serviceDir),
  };
}

// Windows relocation shares this authenticated inbox/start boundary while
// remaining a closed operation union rather than a privileged config RPC.
export {
  admitPendingWindowsRelocationRequest,
  executeWindowsRelocationBrokerCommand,
} from "./windows-relocation-control.ts";
