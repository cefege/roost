import { createHash, randomUUID } from "node:crypto";
import { once } from "node:events";
import { createWriteStream } from "node:fs";
import { mkdir, readFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { finished } from "node:stream/promises";
import { applyPrivateDacl, durableRemove, durableReplace, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "@roost/shared/machine-transaction";
import { roostServiceDir, roostVersionsDir, windowsVersionedBinaryPath } from "@roost/shared/paths";
import { probeServiceHealth } from "@roost/shared/service-health";
import { WINDOWS_SERVICE_NAMES, createWindowsServiceManager, loadWindowsServiceDefinitions, quoteWindowsArg } from "./service-ctl.ts";
import type { RoostServiceRole, WindowsServiceDefinition, WindowsServiceManager, WindowsServiceSnapshot } from "./service-ctl.ts";
import { createWindowsUpdateNative } from "./windows-update-runtime.ts";
import type { WindowsUpdateNative } from "./windows-update-broker.ts";
import { DurableWindowsUpdateJournalStore, createWindowsUpdateJournal, parseWindowsCurrentManifest, parseWindowsReleaseManifest, readWindowsUpdateProgressFromJournal, sha256Hex, windowsCurrentManifestPath } from "./windows-update-journal.ts";
import type { WindowsUpdateJournalStore, WindowsUpdateJournalV1, WindowsUpdateProgressEntry } from "./windows-update-journal.ts";

const SHA256_RE = /^[0-9a-f]{64}$/;
export interface WindowsUpdateBrokerCommand { requestId: string; jobId: string; action: "START" | "STATUS"; manifestUrl: string; signatureUrl: string; manifestSha256: string; publisherSha256: string; afterSequence?: number }
export interface WindowsUpdateProgressFrame { requestId: string; jobId: string; sequence: number; phase: string; message: string; terminal: boolean; success: boolean; error: string }
export interface WindowsUpdateControlDeps { store: WindowsUpdateJournalStore; services: WindowsServiceManager; native: WindowsUpdateNative; fetch: typeof fetch; serviceDir: string; versionsDir: string; currentManifestPath: string; acquireTransaction: typeof acquireMachineTransaction; probeHealth: typeof probeServiceHealth }

export async function handleUpdateBrokerCommand(command: WindowsUpdateBrokerCommand, deps: WindowsUpdateControlDeps = defaultDeps()): Promise<WindowsUpdateProgressFrame[]> {
  validateCommand(command);
  if (command.action === "STATUS") return await readWindowsUpdateProgress(command.jobId, command.afterSequence ?? 0, command.requestId, deps.store);
  if (process.platform !== "win32") throw new Error(`Windows update broker command refused on ${process.platform}`);
  const lock = await deps.acquireTransaction("update", deps.store.path);
  let lockReleased = false;
  try {
    const existing = await deps.store.load();
    if (existing && existing.jobId === command.jobId) {
      const replay = frames(command.requestId, command.jobId, existing.progress);
      const shouldStart = (existing.state === "forward" || existing.state === "rolling-back")
        && (await deps.services.query("updater")).state !== "running";
      await lock.release();
      lockReleased = true;
      if (shouldStart) await deps.services.start("updater");
      return replay;
    }
    if (existing && (existing.state === "forward" || existing.state === "rolling-back")) throw new Error(`Windows update transaction ${existing.jobId} is already active`);
    await applyPrivateDacl(deps.currentManifestPath);
    const currentRaw = await readFile(deps.currentManifestPath, "utf8");
    const current = parseWindowsCurrentManifest(currentRaw);
    const environmentPublisher = (process.env.ROOST_WINDOWS_PUBLISHER_SHA256 ?? "").trim().toLowerCase();
    if (environmentPublisher && environmentPublisher !== current.publisherSha256) {
      throw new Error("protected Windows publisher pins disagree");
    }
    const localPublisher = environmentPublisher || current.publisherSha256;
    const commandPublisher = command.publisherSha256.trim().toLowerCase();
    if (!SHA256_RE.test(localPublisher)) throw new Error("DACL-protected local Windows publisher pin is missing");
    if (commandPublisher && commandPublisher !== localPublisher) throw new Error("remote publisher pin disagrees with protected local pin");

    const stagingDir = join(deps.serviceDir, "updates", createHash("sha256").update(command.jobId).digest("hex"));
    await mkdir(stagingDir, { recursive: true }); await applyPrivateDacl(stagingDir);
    const manifestPath = join(stagingDir, "roost-windows-x64.manifest.json");
    const signaturePath = join(stagingDir, "roost-windows-x64.manifest.json.p7s");
    await downloadDurable(command.manifestUrl, manifestPath, deps.fetch);
    await downloadDurable(command.signatureUrl, signaturePath, deps.fetch);
    const manifestRaw = await readFile(manifestPath);
    if (sha256Hex(manifestRaw) !== command.manifestSha256) throw new Error("downloaded manifest digest does not match command");
    await deps.native.verifyCmsDetached(manifestPath, signaturePath, localPublisher);
    const manifest = parseWindowsReleaseManifest(manifestRaw);
    if (normalizedVersion(manifest.version) === normalizedVersion(current.version)) throw new Error(`already up to date (${manifest.version})`);
    const packageUrl = new URL(`./${manifest.package.name}`, command.manifestUrl).toString();
    const packagePath = join(stagingDir, manifest.package.name);
    await downloadDurable(packageUrl, packagePath, deps.fetch, manifest.package.sha256, manifest.package.size);

    const snapshotSet = await deps.services.snapshot();
    const snapshots = Object.values(snapshotSet); assertSnapshot(snapshots);
    const runningBefore = Object.fromEntries(snapshots.map((snapshot) => [snapshot.role, snapshot.state === "running"])) as Record<RoostServiceRole, boolean>;
    const healthBefore: WindowsUpdateJournalV1["healthBefore"] = {};
    for (const role of ["worker", "coordinator"] as const) {
      if (!runningBefore[role]) continue;
      const health = await deps.probeHealth(role);
      if (health.role !== role) throw new Error(`${role} health probe returned ${health.role} status`);
      if (!health.version || !health.build || !health.processEpoch) throw new Error(`${role} did not provide a complete pre-update health checkpoint`);
      const checkpoint = {
        version: health.version,
        build: health.build,
        processEpoch: health.processEpoch,
      };
      if (health.role === "worker") {
        if (!health.coordinatorUrl) throw new Error("worker did not provide its pre-update coordinator URL");
        healthBefore.worker = { ...checkpoint, coordinatorUrl: health.coordinatorUrl };
      } else {
        healthBefore.coordinator = checkpoint;
      }
    }
    if (runningBefore.updater) throw new Error("RoostUpdaterV2 was already running before journal creation");
    const executablePath = windowsVersionedBinaryPath(manifest.version);
    const newVersionDir = dirname(executablePath);
    if (!isUnder(deps.versionsDir, newVersionDir)) throw new Error("versioned binary path escaped versions directory");
    const installedDefinitions = await loadWindowsServiceDefinitions();
    const shawlPath = join(newVersionDir, "shawl.exe");
    const windowsHelperPath = join(newVersionDir, "roost-win-helper.exe");
    const definitions = Object.fromEntries(
      snapshots.map((snapshot) => [
        snapshot.role,
        rebaseServiceDefinition(
          installedDefinitions[snapshot.role],
          snapshot,
          shawlPath,
          executablePath,
          windowsHelperPath,
        ),
      ]),
    ) as Record<RoostServiceRole, WindowsServiceDefinition>;
    const next = { schemaVersion: 1 as const, version: manifest.version, versionDir: newVersionDir, files: manifest.files.map(({ path, sha256, size }) => ({ path, sha256, size })), manifestUrl: command.manifestUrl, manifestSha256: command.manifestSha256, publisherSha256: localPublisher };
    const journal = createWindowsUpdateJournal({ jobId: command.jobId, targetVersion: manifest.version, signedManifest: { url: command.manifestUrl, signatureUrl: command.signatureUrl, path: manifestPath, signaturePath, sha256: command.manifestSha256, publisherSha256: localPublisher }, releasePackage: { url: packageUrl, path: packagePath, sha256: manifest.package.sha256, size: manifest.package.size }, assets: manifest.files, paths: { priorVersionDir: current.versionDir, newVersionDir, stagingDir, currentManifestPath: deps.currentManifestPath }, currentManifest: { priorRaw: currentRaw, next }, serviceSnapshot: snapshotSet, priorServiceDefinitions: installedDefinitions, nextServiceDefinitions: Object.values(definitions), runningBefore, healthBefore });
    await deps.store.save(journal);
    const result = frames(command.requestId, command.jobId, journal.progress);
    // Release before SCM launch: the broker acquires this same fail-closed
    // machine transaction lock as its first operation.
    await lock.release();
    lockReleased = true;
    // Worker asks SCM only: never self-spawn a broker and never self-stop.
    await deps.services.start("updater");
    return result;
  } finally {
    if (!lockReleased) await lock.release();
  }
}

export async function readWindowsUpdateProgress(jobId: string, afterSequence = 0, requestId = "status", store: WindowsUpdateJournalStore = new DurableWindowsUpdateJournalStore()): Promise<WindowsUpdateProgressFrame[]> {
  if (!jobId) throw new Error("jobId is required");
  const journal = await store.load();
  return !journal || journal.jobId !== jobId ? [] : frames(requestId, jobId, readWindowsUpdateProgressFromJournal(journal, afterSequence));
}
export function rebaseServiceDefinition(
  installed: WindowsServiceDefinition,
  snapshot: WindowsServiceSnapshot,
  shawlPath: string,
  executablePath: string,
  windowsHelperPath: string,
): WindowsServiceDefinition {
  if (!snapshot.installed || !snapshot.imagePath || !snapshot.account) {
    throw new Error(`${snapshot.name} lacks an installed structured service definition`);
  }
  if (installed.imagePath !== snapshot.imagePath) {
    throw new Error(`${snapshot.name} differs from its protected service definition; refusing update`);
  }
  if (snapshot.startMode !== "automatic" && snapshot.startMode !== "manual") {
    throw new Error(`${snapshot.name} has unsupported start mode ${snapshot.startMode}`);
  }
  const shawlArguments = [...installed.shawlArguments];
  const separator = shawlArguments.indexOf("--");
  if (separator < 1 || separator + 1 >= shawlArguments.length) {
    throw new Error(`${snapshot.name} has an invalid structured Shawl argv`);
  }
  shawlArguments[separator + 1] = executablePath;
  const environment = { ...installed.environment };
  if (snapshot.role === "keeper" || snapshot.role === "worker") {
    environment.ROOST_WIN_HELPER = windowsHelperPath;
    let found = false;
    for (let index = 0; index < separator; index += 1) {
      if (shawlArguments[index] !== "--env") continue;
      const assignment = shawlArguments[++index];
      if (assignment.slice(0, assignment.indexOf("=")).toUpperCase() !== "ROOST_WIN_HELPER") continue;
      shawlArguments[index] = `ROOST_WIN_HELPER=${windowsHelperPath}`;
      found = true;
    }
    if (!found) shawlArguments.splice(separator, 0, "--env", `ROOST_WIN_HELPER=${windowsHelperPath}`);
  }
  const dependencies = snapshot.dependencies.map((name) => {
    const role = (Object.entries(WINDOWS_SERVICE_NAMES) as Array<[RoostServiceRole, string]>)
      .find(([, serviceName]) => serviceName.toLowerCase() === name.toLowerCase())?.[0];
    if (!role) throw new Error(`${snapshot.name} has unknown dependency ${name}`);
    return role;
  });
  return {
    ...installed,
    startMode: snapshot.startMode,
    account: snapshot.account,
    dependencies,
    shawlPath,
    shawlArguments,
    executablePath,
    environment,
    imagePath: [shawlPath, ...shawlArguments].map(quoteWindowsArg).join(" "),
  };
}


function defaultDeps(): WindowsUpdateControlDeps { const serviceDir = roostServiceDir(); return { store: new DurableWindowsUpdateJournalStore(), services: createWindowsServiceManager(), native: createWindowsUpdateNative(), fetch, serviceDir, versionsDir: roostVersionsDir(), currentManifestPath: windowsCurrentManifestPath(serviceDir), acquireTransaction: acquireMachineTransaction, probeHealth: probeServiceHealth }; }
async function downloadDurable(url: string, destination: string, fetchImpl: typeof fetch, expectedSha256?: string, expectedSize?: number): Promise<void> {
  requireHttps(url, "download URL");
  const response = await fetchImpl(url, { signal: AbortSignal.timeout(120_000) });
  if (!response.ok || !response.body) throw new Error(`download failed for ${url}: HTTP ${response.status}`);
  await mkdir(dirname(destination), { recursive: true });
  const temp = `${destination}.${process.pid}.${randomUUID()}.tmp`;
  const output = createWriteStream(temp, { flags: "wx", mode: 0o600 });
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
    await flushDurablePath(temp);
    await applyPrivateDacl(temp);
    if (expectedSize !== undefined && bytes !== expectedSize) throw new Error(`download size mismatch for ${url}`);
    if (expectedSha256 !== undefined && hash.digest("hex") !== expectedSha256) throw new Error(`download digest mismatch for ${url}`);
    await durableReplace(temp, destination);
    await applyPrivateDacl(destination);
    await flushDurablePath(dirname(destination));
  } catch (error) {
    output.destroy();
    throw error;
  } finally {
    reader.releaseLock();
    await durableRemove(temp).catch(() => {});
  }
}
function frames(requestId: string, jobId: string, entries: readonly WindowsUpdateProgressEntry[]): WindowsUpdateProgressFrame[] { return entries.map((e) => ({ requestId, jobId, sequence: e.sequence, phase: e.phase, message: e.message, terminal: e.terminal, success: e.success, error: e.error ?? "" })); }
function validateCommand(c: WindowsUpdateBrokerCommand): void { if (!c.requestId || !c.jobId) throw new Error("requestId/jobId required"); if (c.action !== "START" && c.action !== "STATUS") throw new Error("unknown update action"); if (c.action === "START") { requireHttps(c.manifestUrl, "manifestUrl"); requireHttps(c.signatureUrl, "signatureUrl"); if (!SHA256_RE.test(c.manifestSha256)) throw new Error("invalid manifestSha256"); if (c.publisherSha256 && !SHA256_RE.test(c.publisherSha256.trim().toLowerCase())) throw new Error("invalid publisherSha256"); } }
function assertSnapshot(s: readonly WindowsServiceSnapshot[]): void { const roles: readonly RoostServiceRole[] = ["keeper", "worker", "coordinator", "updater"]; if (s.length !== 4 || roles.some((role) => !s.some((v) => v.role === role))) throw new Error("service snapshot is not exact four-role vector"); }
function requireHttps(value: string, label: string): void { let url: URL; try { url = new URL(value); } catch { throw new Error(`${label} must be absolute`); } if (url.protocol !== "https:") throw new Error(`${label} must use HTTPS`); }
function normalizedVersion(v: string): string { return v.replace(/^v/, "").split("+")[0]; }
function isUnder(parent: string, child: string): boolean { const p = parent.replaceAll("/", "\\").replace(/\\+$/, "").toLowerCase(); const c = child.replaceAll("/", "\\").toLowerCase(); return c.startsWith(`${p}\\`); }
