import { createHash, randomUUID } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readFile, readdir } from "node:fs/promises";
import { dirname, join, relative, resolve } from "node:path";
import { applyPrivateDacl, durableRemove, durableReplace, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { acquireMachineTransaction } from "@roost/shared/machine-transaction";
import { storeWindowsServiceDefinitions } from "./service-ctl.ts";
import type { RoostServiceRole, WindowsServiceDefinition, WindowsServiceSnapshot, WindowsServiceSnapshotSet } from "./service-ctl.ts";
import { appendWindowsUpdateProgress, parseWindowsReleaseManifest, sha256Hex } from "./windows-update-journal.ts";
import type { WindowsReleaseFile, WindowsUpdateForwardPhase, WindowsUpdateJournalStore, WindowsUpdateJournalV1 } from "./windows-update-journal.ts";

const ACTIVE_ROLES = ["coordinator", "worker"] as const satisfies readonly RoostServiceRole[];
const FORWARD_ORDER: readonly WindowsUpdateForwardPhase[] = ["prepared", "broker-started", "assets-staged", "services-stopped", "service-configs-switched", "current-manifest-switched", "services-restored", "health-proven", "committed"];
export interface WindowsUpdateServiceManager {
  query(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  configure(definition: WindowsServiceDefinition): Promise<WindowsServiceSnapshot>;
  start(role: RoostServiceRole): Promise<WindowsServiceSnapshot>;
  stop(role: RoostServiceRole, options?: { timeoutMs?: number }): Promise<WindowsServiceSnapshot>;
  restore(snapshot: WindowsServiceSnapshotSet, options?: { restoreLifecycleRoles?: readonly RoostServiceRole[] }): Promise<WindowsServiceSnapshotSet>;
}
export interface WindowsUpdateNative {
  assertUpdaterServiceContext(): Promise<void>;
  verifyCmsDetached(manifestPath: string, signaturePath: string, publisherSha256: string): Promise<void>;
  verifyAuthenticode(path: string, publisherSha256: string): Promise<void>;
  extractZip(packagePath: string, destination: string, files: readonly WindowsReleaseFile[]): Promise<void>;
}
export interface ServiceHealthProver {
  prove(role: "worker" | "coordinator", journal: Readonly<WindowsUpdateJournalV1>, mode: "forward" | "rollback"): Promise<void>;
}
export interface WindowsUpdateBrokerDeps { store: WindowsUpdateJournalStore; services: WindowsUpdateServiceManager; native: WindowsUpdateNative; health: ServiceHealthProver; acquireTransaction?: typeof acquireMachineTransaction; now?: () => Date }

export async function runWindowsUpdateBroker(deps: WindowsUpdateBrokerDeps): Promise<WindowsUpdateJournalV1> {
  await deps.native.assertUpdaterServiceContext();
  const transaction = await (deps.acquireTransaction ?? acquireMachineTransaction)("update", deps.store.path);
  try {
    let journal = await deps.store.load();
    if (!journal) throw new Error(`SCM updater started without a durable journal at ${deps.store.path}`);
    if (journal.state === "succeeded" || journal.state === "rolled-back") return journal;
    if (journal.state === "rolling-back") return await rollback(journal, deps);
    try {
      if (journal.phase === "prepared") journal = await checkpoint(journal, deps, "broker-started", "SCM-launched updater resumed the durable journal");
      if (before(journal, "assets-staged")) { await stageAndVerifyAssets(journal, deps.native); journal = await checkpoint(journal, deps, "assets-staged", "manifest, package, assets, and signatures verified"); }
      if (before(journal, "services-stopped")) {
        const stopped: RoostServiceRole[] = [];
        for (const role of ACTIVE_ROLES) if (journal.runningBefore[role]) { await deps.services.stop(role, { timeoutMs: 15_000 }); stopped.push(role); }
        journal = { ...journal, stoppedRoles: uniqueRoles([...journal.stoppedRoles, ...stopped]) };
        journal = await checkpoint(journal, deps, "services-stopped", stopped.length ? `stopped prior-active roles: ${stopped.join(", ")}` : "no active worker/coordinator required stopping");
      }
      if (before(journal, "service-configs-switched")) {
        for (const definition of orderedDefinitions(journal.nextServiceDefinitions)) await deps.services.configure(definition);
        await storeWindowsServiceDefinitions(definitionRecord(journal.nextServiceDefinitions));
        journal = await checkpoint(journal, deps, "service-configs-switched", "service configs switched durably; keeper/updater remained live");
      }
      if (before(journal, "current-manifest-switched")) { await replaceProtectedText(journal.paths.currentManifestPath, `${JSON.stringify(journal.currentManifest.next)}\n`); journal = await checkpoint(journal, deps, "current-manifest-switched", "current manifest switched durably"); }
      if (before(journal, "services-restored")) { const roles = await startPriorActiveRoles(journal, deps.services); journal = { ...journal, restoredRoles: uniqueRoles([...journal.restoredRoles, ...roles]) }; journal = await checkpoint(journal, deps, "services-restored", roles.length ? `restored prior-active roles: ${roles.join(", ")}` : "prior vector required no starts"); }
      if (before(journal, "health-proven")) { for (const role of ACTIVE_ROLES) if (journal.runningBefore[role]) await deps.health.prove(role, journal, "forward"); journal = await checkpoint(journal, deps, "health-proven", "endpoint health and worker reconnect proofs passed"); }
      if (before(journal, "committed")) { journal = appendWindowsUpdateProgress(journal, "committed", `Windows update to ${journal.targetVersion} committed`, { state: "succeeded", terminal: true, success: true, now: now(deps) }); await deps.store.save(journal); }
      return journal;
    } catch (error) {
      const forwardPhase = isForward(journal.phase) ? journal.phase : "broker-started";
      journal = { ...journal, failure: { forwardPhase, error: errorText(error), at: now(deps).toISOString() } };
      journal = appendWindowsUpdateProgress(journal, "rollback-started", `forward update failed at ${forwardPhase}; rolling back`, { state: "rolling-back", error: errorText(error), now: now(deps) });
      await deps.store.save(journal); return await rollback(journal, deps);
    }
  } finally { await transaction.release(); }
}

async function rollback(initial: WindowsUpdateJournalV1, deps: WindowsUpdateBrokerDeps): Promise<WindowsUpdateJournalV1> {
  let journal = initial;
  try {
    const touched = phaseAtLeast(journal.failure?.forwardPhase ?? "prepared", "services-stopped");
    if (rollbackBefore(journal, "rollback-services-stopped")) { if (touched) for (const role of ACTIVE_ROLES) if (journal.runningBefore[role]) await deps.services.stop(role, { timeoutMs: 15_000 }); journal = await checkpoint(journal, deps, "rollback-services-stopped", touched ? "stopped only roles active before update" : "services were untouched before failure", "rolling-back"); }
    if (rollbackBefore(journal, "rollback-configs-restored")) { await deps.services.restore(journal.serviceSnapshot, { restoreLifecycleRoles: [] }); await storeWindowsServiceDefinitions(journal.priorServiceDefinitions); await rollbackVersionAssets(journal); journal = await checkpoint(journal, deps, "rollback-configs-restored", "restored exact service config without keeper/updater lifecycle", "rolling-back"); }
    if (rollbackBefore(journal, "rollback-current-manifest-restored")) { if (journal.currentManifest.priorRaw === null) { await durableRemove(journal.paths.currentManifestPath); await flushDurablePath(dirname(journal.paths.currentManifestPath)); } else await replaceProtectedText(journal.paths.currentManifestPath, journal.currentManifest.priorRaw); journal = await checkpoint(journal, deps, "rollback-current-manifest-restored", "restored exact prior current manifest", "rolling-back"); }
    if (rollbackBefore(journal, "rollback-services-restored")) { const roles = await startPriorActiveRoles(journal, deps.services); for (const role of ACTIVE_ROLES) if (journal.runningBefore[role]) await deps.health.prove(role, journal, "rollback"); journal = { ...journal, restoredRoles: uniqueRoles([...journal.restoredRoles, ...roles]) }; journal = await checkpoint(journal, deps, "rollback-services-restored", "restored and proved exact prior active vector", "rolling-back"); }
    journal = appendWindowsUpdateProgress(journal, "rolled-back", "Windows update rolled back deterministically", { state: "rolled-back", terminal: true, success: false, error: journal.failure?.error, now: now(deps) }); await deps.store.save(journal); return journal;
  } catch (error) {
    const phase = isRollback(journal.phase) ? journal.phase : "rollback-started";
    journal = { ...journal, rollbackFailure: { phase, error: errorText(error), at: now(deps).toISOString() } };
    journal = appendWindowsUpdateProgress(journal, phase, "rollback interrupted; SCM restart will resume this journal", { state: "rolling-back", error: errorText(error), now: now(deps) }); await deps.store.save(journal); throw error;
  }
}

async function stageAndVerifyAssets(journal: WindowsUpdateJournalV1, native: WindowsUpdateNative): Promise<void> {
  const raw = await readFile(journal.signedManifest.path);
  if (sha256Hex(raw) !== journal.signedManifest.sha256) throw new Error("staged manifest digest changed");
  await native.verifyCmsDetached(journal.signedManifest.path, journal.signedManifest.signaturePath, journal.signedManifest.publisherSha256);
  const manifest = parseWindowsReleaseManifest(raw);
  if (manifest.version !== journal.targetVersion || manifest.package.sha256 !== journal.releasePackage.sha256 || manifest.package.size !== journal.releasePackage.size || JSON.stringify(manifest.files) !== JSON.stringify(journal.assets)) {
    throw new Error("journal does not match signed manifest");
  }
  await verifyFile(journal.releasePackage.path, journal.releasePackage.sha256, journal.releasePackage.size);
  if (await directoryExists(journal.paths.newVersionDir)) {
    await verifyTree(journal.paths.newVersionDir, journal.assets, journal.signedManifest.publisherSha256, native);
    return;
  }
  const extracted = join(journal.paths.stagingDir, `extracted-${process.pid}-${randomUUID()}`);
  await mkdir(extracted, { recursive: true });
  await applyPrivateDacl(extracted);
  await native.extractZip(journal.releasePackage.path, extracted, journal.assets);
  await verifyTree(extracted, journal.assets, journal.signedManifest.publisherSha256, native);
  await durableReplace(extracted, journal.paths.newVersionDir);
  await applyPrivateDacl(journal.paths.newVersionDir);
  await flushDurablePath(dirname(journal.paths.newVersionDir));
  await verifyTree(journal.paths.newVersionDir, journal.assets, journal.signedManifest.publisherSha256, native);
}
async function verifyTree(root: string, assets: readonly WindowsReleaseFile[], publisher: string, native: WindowsUpdateNative): Promise<void> {
  const expected = new Set(assets.map((a) => a.path.replaceAll("\\", "/").toLowerCase())); const actual = await listFiles(root);
  for (const path of actual) if (!expected.has(path.toLowerCase())) throw new Error(`unmanifested archive asset: ${path}`); if (actual.length !== expected.size) throw new Error("archive asset count mismatch");
  for (const asset of assets) { const path = resolveUnder(root, asset.path); await verifyFile(path, asset.sha256, asset.size); if (asset.authenticodeRequired) await native.verifyAuthenticode(path, publisher); }
}
async function verifyFile(path: string, expected: string, bytes: number): Promise<void> { const info = await lstat(path); if (!info.isFile() || info.isSymbolicLink() || info.size !== bytes) throw new Error(`asset metadata mismatch: ${path}`); const hash = createHash("sha256"); for await (const chunk of createReadStream(path)) hash.update(chunk as Buffer); if (hash.digest("hex") !== expected) throw new Error(`asset digest mismatch: ${path}`); }
async function listFiles(root: string, prefix = ""): Promise<string[]> { const result: string[] = []; for (const entry of await readdir(join(root, prefix), { withFileTypes: true })) { const path = prefix ? `${prefix}/${entry.name}` : entry.name; if (entry.isSymbolicLink()) throw new Error(`archive link/reparse asset: ${path}`); if (entry.isDirectory()) result.push(...await listFiles(root, path)); else if (entry.isFile()) result.push(path); else throw new Error(`unsupported archive asset: ${path}`); } return result; }
async function rollbackVersionAssets(journal: WindowsUpdateJournalV1): Promise<void> {
  const versionDir = journal.paths.newVersionDir;
  if (versionDir === journal.paths.priorVersionDir || !await directoryExists(versionDir)) return;
  const folded = versionDir.replaceAll("/", "\\").toLowerCase();
  const referenced = Object.values(journal.serviceSnapshot).some((snapshot) =>
    snapshot.imagePath?.replaceAll("/", "\\").toLowerCase().includes(folded),
  ) || process.execPath.replaceAll("/", "\\").toLowerCase().startsWith(`${folded}\\`);
  if (referenced) return;
  const quarantine = join(journal.paths.stagingDir, "rolled-back-version");
  if (await directoryExists(quarantine)) return;
  await mkdir(journal.paths.stagingDir, { recursive: true });
  await durableReplace(versionDir, quarantine);
  await flushDurablePath(dirname(versionDir));
}
async function replaceProtectedText(path: string, contents: string): Promise<void> { const parent = dirname(path); await mkdir(parent, { recursive: true }); await applyPrivateDacl(parent); const temp = `${path}.${process.pid}.${randomUUID()}.tmp`; try { await durableWriteFile(temp, contents); await applyPrivateDacl(temp); await durableReplace(temp, path); await applyPrivateDacl(path); await flushDurablePath(parent); } finally { await durableRemove(temp).catch(() => {}); } }
async function startPriorActiveRoles(journal: WindowsUpdateJournalV1, services: WindowsUpdateServiceManager): Promise<RoostServiceRole[]> { const restored: RoostServiceRole[] = []; for (const role of ACTIVE_ROLES) if (journal.runningBefore[role]) { if ((await services.query(role)).state !== "running") await services.start(role); restored.push(role); } return restored; }
async function checkpoint(journal: WindowsUpdateJournalV1, deps: WindowsUpdateBrokerDeps, phase: Parameters<typeof appendWindowsUpdateProgress>[1], message: string, state: "forward" | "rolling-back" = "forward"): Promise<WindowsUpdateJournalV1> { const next = appendWindowsUpdateProgress(journal, phase, message, { state, now: now(deps) }); await deps.store.save(next); return next; }
function orderedDefinitions(definitions: readonly WindowsServiceDefinition[]): WindowsServiceDefinition[] { const order: readonly RoostServiceRole[] = ["worker", "coordinator", "keeper", "updater"]; return order.map((role) => { const d = definitions.find((v) => v.role === role); if (!d) throw new Error(`missing next definition for ${role}`); return d; }); }
function definitionRecord(definitions: readonly WindowsServiceDefinition[]): Readonly<Record<RoostServiceRole, WindowsServiceDefinition>> {
  return Object.fromEntries(orderedDefinitions(definitions).map((definition) => [definition.role, definition])) as Record<RoostServiceRole, WindowsServiceDefinition>;
}
function resolveUnder(root: string, path: string): string { const target = resolve(root, path); const rel = relative(resolve(root), target); if (!rel || rel.startsWith("..") || resolve(root, rel) !== target) throw new Error(`asset escapes version directory: ${path}`); return target; }
async function directoryExists(path: string): Promise<boolean> { try { return (await lstat(path)).isDirectory(); } catch (error) { if (nodeError(error)?.code === "ENOENT") return false; throw error; } }
function before(j: WindowsUpdateJournalV1, p: WindowsUpdateForwardPhase): boolean { return isForward(j.phase) && FORWARD_ORDER.indexOf(j.phase) < FORWARD_ORDER.indexOf(p); }
function phaseAtLeast(a: WindowsUpdateForwardPhase, b: WindowsUpdateForwardPhase): boolean { return FORWARD_ORDER.indexOf(a) >= FORWARD_ORDER.indexOf(b); }
function rollbackBefore(j: WindowsUpdateJournalV1, phase: WindowsUpdateJournalV1["phase"]): boolean { const order = ["rollback-started", "rollback-services-stopped", "rollback-configs-restored", "rollback-current-manifest-restored", "rollback-services-restored", "rolled-back"]; return order.indexOf(j.phase) < order.indexOf(phase); }
function isForward(p: WindowsUpdateJournalV1["phase"]): p is WindowsUpdateForwardPhase { return FORWARD_ORDER.includes(p as WindowsUpdateForwardPhase); }
function isRollback(p: WindowsUpdateJournalV1["phase"]): p is Exclude<WindowsUpdateJournalV1["phase"], WindowsUpdateForwardPhase> { return !isForward(p); }
function uniqueRoles(roles: readonly RoostServiceRole[]): RoostServiceRole[] { return [...new Set(roles)]; }
function errorText(error: unknown): string { return String(error).replace(/[\r\n]+/g, " ").slice(0, 2048); }
function now(deps: WindowsUpdateBrokerDeps): Date { return (deps.now ?? (() => new Date()))(); }
function nodeError(error: unknown): NodeJS.ErrnoException | null { return error instanceof Error && "code" in error ? error as NodeJS.ErrnoException : null; }
