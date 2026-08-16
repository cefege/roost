import { createHash, randomUUID } from "node:crypto";
import { mkdir, readFile } from "node:fs/promises";
import { dirname, isAbsolute, join } from "node:path";
import { applyPrivateDacl, durableRemove, durableReplace, durableWriteFile, flushDurablePath } from "@roost/shared/durability";
import { roostServiceDir } from "@roost/shared/paths";
import type { RoostServiceRole, WindowsServiceDefinition, WindowsServiceSnapshotSet } from "./service-ctl.ts";

export const WINDOWS_UPDATE_JOURNAL_SCHEMA_VERSION = 1 as const;
export const WINDOWS_CURRENT_MANIFEST_SCHEMA_VERSION = 1 as const;
export const MAX_WINDOWS_UPDATE_PROGRESS = 128;
export const WINDOWS_UPDATE_JOURNAL_FILE = "update-v1.json";
export const WINDOWS_CURRENT_MANIFEST_FILE = "current.json";
const SHA256_RE = /^[0-9a-f]{64}$/;
const SERVICE_ROLES = ["keeper", "worker", "coordinator", "updater"] as const satisfies readonly RoostServiceRole[];

export type WindowsUpdateForwardPhase = "prepared" | "broker-started" | "assets-staged" | "services-stopped" | "service-configs-switched" | "current-manifest-switched" | "services-restored" | "health-proven" | "committed";
export type WindowsUpdateRollbackPhase = "rollback-started" | "rollback-services-stopped" | "rollback-configs-restored" | "rollback-current-manifest-restored" | "rollback-services-restored" | "rolled-back";
export type WindowsUpdatePhase = WindowsUpdateForwardPhase | WindowsUpdateRollbackPhase;
export type WindowsUpdateState = "forward" | "rolling-back" | "succeeded" | "rolled-back";

export interface WindowsReleaseFile { path: string; sha256: string; size: number; authenticodeRequired: boolean }
/** Detached CMS covers the exact raw bytes containing this JSON. */
export interface WindowsReleaseManifestV1 {
  schemaVersion: 1;
  version: string;
  platform: "win32";
  arch: "x64";
  publishedAt: string;
  package: { name: "roost-windows-x64.zip"; sha256: string; size: number };
  files: WindowsReleaseFile[];
  shawl: { version: "1.9.0"; upstreamSha256: string };
}
export interface WindowsCurrentManifestV1 {
  schemaVersion: 1;
  version: string;
  versionDir: string;
  files: Array<Pick<WindowsReleaseFile, "path" | "sha256" | "size">>;
  manifestUrl: string;
  manifestSha256: string;
  publisherSha256: string;
}
export interface WindowsUpdateProgressEntry {
  sequence: number;
  at: string;
  phase: WindowsUpdatePhase;
  message: string;
  terminal: boolean;
  success: boolean;
  error?: string;
}
export interface WindowsServiceHealthCheckpoint {
  version: string;
  build: string;
  processEpoch: string;
  coordinatorUrl?: string;
}
export interface WindowsUpdateJournalV1 {
  schemaVersion: 1;
  transactionId: string;
  jobId: string;
  createdAt: string;
  updatedAt: string;
  revision: number;
  state: WindowsUpdateState;
  phase: WindowsUpdatePhase;
  targetVersion: string;
  signedManifest: { url: string; signatureUrl: string; path: string; signaturePath: string; sha256: string; publisherSha256: string };
  releasePackage: { url: string; path: string; sha256: string; size: number };
  assets: WindowsReleaseFile[];
  paths: { priorVersionDir: string | null; newVersionDir: string; stagingDir: string; currentManifestPath: string };
  currentManifest: { priorRaw: string | null; next: WindowsCurrentManifestV1 };
  serviceSnapshot: WindowsServiceSnapshotSet;
  priorServiceDefinitions: Readonly<Record<RoostServiceRole, WindowsServiceDefinition>>;
  nextServiceDefinitions: WindowsServiceDefinition[];
  runningBefore: Record<RoostServiceRole, boolean>;
  healthBefore: Partial<Record<"worker" | "coordinator", WindowsServiceHealthCheckpoint>>;
  stoppedRoles: RoostServiceRole[];
  restoredRoles: RoostServiceRole[];
  retainedVersionDirs: string[];
  progress: WindowsUpdateProgressEntry[];
  failure?: { forwardPhase: WindowsUpdateForwardPhase; error: string; at: string };
  rollbackFailure?: { phase: WindowsUpdateRollbackPhase; error: string; at: string };
}
export interface CreateWindowsUpdateJournalInput {
  jobId?: string;
  targetVersion: string;
  signedManifest: WindowsUpdateJournalV1["signedManifest"];
  releasePackage: WindowsUpdateJournalV1["releasePackage"];
  assets: WindowsReleaseFile[];
  paths: WindowsUpdateJournalV1["paths"];
  currentManifest: WindowsUpdateJournalV1["currentManifest"];
  serviceSnapshot: WindowsServiceSnapshotSet;
  priorServiceDefinitions: WindowsUpdateJournalV1["priorServiceDefinitions"];
  nextServiceDefinitions: WindowsServiceDefinition[];
  runningBefore: Record<RoostServiceRole, boolean>;
  healthBefore: WindowsUpdateJournalV1["healthBefore"];
  now?: () => Date;
  transactionId?: string;
}
export interface WindowsUpdateJournalStore {
  readonly path: string;
  load(): Promise<WindowsUpdateJournalV1 | null>;
  save(journal: WindowsUpdateJournalV1): Promise<void>;
}

export function windowsUpdateJournalPath(serviceDir: string = roostServiceDir()): string { return join(serviceDir, WINDOWS_UPDATE_JOURNAL_FILE); }
export function windowsCurrentManifestPath(serviceDir: string = roostServiceDir()): string { return join(serviceDir, WINDOWS_CURRENT_MANIFEST_FILE); }
export function sha256Hex(bytes: string | Uint8Array): string { return createHash("sha256").update(bytes).digest("hex"); }

export function parseWindowsReleaseManifest(raw: string | Uint8Array): WindowsReleaseManifestV1 {
  const o = record(parseJson(typeof raw === "string" ? raw : Buffer.from(raw).toString("utf8"), "Windows release manifest"), "Windows release manifest");
  if (o.schemaVersion !== 1) throw new Error(`unsupported Windows release manifest schema: ${String(o.schemaVersion)}`);
  if (o.platform !== "win32" || o.arch !== "x64") throw new Error(`manifest targets ${String(o.platform)}/${String(o.arch)}, expected win32/x64`);
  const version = nonempty(o.version, "manifest.version");
  if (/[/\\\0]/.test(version) || version === "." || version === "..") throw new Error("manifest.version is unsafe");
  const publishedAt = nonempty(o.publishedAt, "manifest.publishedAt");
  if (!Number.isFinite(Date.parse(publishedAt))) throw new Error("manifest.publishedAt is invalid");
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
    if (typeof f.authenticodeRequired !== "boolean") throw new Error(`manifest.files[${index}].authenticodeRequired is invalid`);
    return { path, sha256: sha(f.sha256, `manifest.files[${index}].sha256`), size: size(f.size, `manifest.files[${index}].size`), authenticodeRequired: f.authenticodeRequired };
  });
  for (const required of ["roost.exe", "roost-win-helper.exe", "shawl.exe"]) if (!seen.has(required)) throw new Error(`manifest is missing ${required}`);
  const shawl = record(o.shawl, "manifest.shawl");
  if (shawl.version !== "1.9.0") throw new Error(`unsupported Shawl version: ${String(shawl.version)}`);
  const upstreamSha256 = sha(shawl.upstreamSha256, "manifest.shawl.upstreamSha256");
  if (files.find((f) => f.path.toLowerCase() === "shawl.exe")?.sha256 !== upstreamSha256) throw new Error("Shawl provenance digest mismatch");
  return { schemaVersion: 1, version, platform: "win32", arch: "x64", publishedAt, package: { name: "roost-windows-x64.zip", sha256: sha(pkg.sha256, "manifest.package.sha256"), size: size(pkg.size, "manifest.package.size") }, files, shawl: { version: "1.9.0", upstreamSha256 } };
}

export function parseWindowsCurrentManifest(raw: string): WindowsCurrentManifestV1 {
  const o = record(parseJson(raw, "Windows current manifest"), "Windows current manifest");
  if (o.schemaVersion !== 1) throw new Error(`unsupported Windows current manifest schema: ${String(o.schemaVersion)}`);
  if (!Array.isArray(o.files)) throw new Error("current manifest files must be an array");
  return { schemaVersion: 1, version: nonempty(o.version, "current version"), versionDir: nonempty(o.versionDir, "current versionDir"), files: o.files.map((value, i) => { const f = record(value, `current files[${i}]`); return { path: safeRelative(f.path, `current files[${i}].path`), sha256: sha(f.sha256, `current files[${i}].sha256`), size: size(f.size, `current files[${i}].size`) }; }), manifestUrl: httpsUrl(o.manifestUrl, "current manifestUrl"), manifestSha256: sha(o.manifestSha256, "current manifestSha256"), publisherSha256: normalizedSha(o.publisherSha256, "current publisherSha256") };
}

export function createWindowsUpdateJournal(input: CreateWindowsUpdateJournalInput): WindowsUpdateJournalV1 {
  const now = (input.now ?? (() => new Date()))().toISOString();
  const transactionId = input.transactionId ?? randomUUID();
  const journal: WindowsUpdateJournalV1 = { schemaVersion: 1, transactionId, jobId: input.jobId ?? transactionId, createdAt: now, updatedAt: now, revision: 1, state: "forward", phase: "prepared", targetVersion: input.targetVersion, signedManifest: input.signedManifest, releasePackage: input.releasePackage, assets: input.assets, paths: input.paths, currentManifest: input.currentManifest, serviceSnapshot: input.serviceSnapshot, priorServiceDefinitions: input.priorServiceDefinitions, nextServiceDefinitions: input.nextServiceDefinitions, runningBefore: input.runningBefore, healthBefore: input.healthBefore, stoppedRoles: [], restoredRoles: [], retainedVersionDirs: input.paths.priorVersionDir ? [input.paths.priorVersionDir] : [], progress: [{ sequence: 1, at: now, phase: "prepared", message: "release package staged and update journal committed", terminal: false, success: false }] };
  assertWindowsUpdateJournal(journal); return journal;
}
export function appendWindowsUpdateProgress(journal: WindowsUpdateJournalV1, phase: WindowsUpdatePhase, message: string, options: { state?: WindowsUpdateState; terminal?: boolean; success?: boolean; error?: string; now?: Date } = {}): WindowsUpdateJournalV1 {
  const at = (options.now ?? new Date()).toISOString();
  const entry: WindowsUpdateProgressEntry = { sequence: (journal.progress.at(-1)?.sequence ?? 0) + 1, at, phase, message: bounded(message), terminal: options.terminal ?? false, success: options.success ?? false, ...(options.error ? { error: bounded(options.error) } : {}) };
  const next: WindowsUpdateJournalV1 = { ...journal, phase, state: options.state ?? journal.state, updatedAt: at, revision: journal.revision + 1, progress: [...journal.progress, entry].slice(-MAX_WINDOWS_UPDATE_PROGRESS) };
  assertWindowsUpdateJournal(next); return next;
}
export function readWindowsUpdateProgressFromJournal(journal: WindowsUpdateJournalV1 | null, afterSequence = 0): WindowsUpdateProgressEntry[] {
  if (!Number.isSafeInteger(afterSequence) || afterSequence < 0) throw new Error("afterSequence must be non-negative");
  return journal ? journal.progress.filter((entry) => entry.sequence > afterSequence) : [];
}

export function assertWindowsUpdateJournal(value: unknown): asserts value is WindowsUpdateJournalV1 {
  const j = record(value, "Windows update journal");
  if (j.schemaVersion !== 1) throw new Error(`unsupported Windows update journal schema: ${String(j.schemaVersion)}`);
  nonempty(j.transactionId, "journal.transactionId"); nonempty(j.jobId, "journal.jobId"); nonempty(j.targetVersion, "journal.targetVersion");
  if (!Number.isSafeInteger(j.revision) || (j.revision as number) < 1) throw new Error("journal.revision is invalid");
  if (!isState(j.state) || !isPhase(j.phase)) throw new Error("journal state/phase is invalid");
  const signed = record(j.signedManifest, "journal.signedManifest"); httpsUrl(signed.url, "manifest URL"); httpsUrl(signed.signatureUrl, "signature URL"); nonempty(signed.path, "manifest path"); nonempty(signed.signaturePath, "signature path"); sha(signed.sha256, "manifest digest"); sha(signed.publisherSha256, "publisher digest");
  const pkg = record(j.releasePackage, "journal.releasePackage"); httpsUrl(pkg.url, "package URL"); nonempty(pkg.path, "package path"); sha(pkg.sha256, "package digest"); size(pkg.size, "package size");
  if (!Array.isArray(j.assets) || j.assets.length === 0) throw new Error("journal assets are missing");
  for (const [i, value] of j.assets.entries()) { const a = record(value, `journal.assets[${i}]`); safeRelative(a.path, `journal.assets[${i}].path`); sha(a.sha256, `journal.assets[${i}].sha256`); size(a.size, `journal.assets[${i}].size`); if (typeof a.authenticodeRequired !== "boolean") throw new Error("invalid Authenticode flag"); }
  const paths = record(j.paths, "journal.paths"); for (const k of ["newVersionDir", "stagingDir", "currentManifestPath"] as const) nonempty(paths[k], `journal.paths.${k}`); if (paths.priorVersionDir !== null) nonempty(paths.priorVersionDir, "journal.paths.priorVersionDir");
  const current = record(j.currentManifest, "journal.currentManifest");
  if (current.priorRaw !== null && typeof current.priorRaw !== "string") throw new Error("journal prior current manifest is invalid");
  if (typeof current.priorRaw === "string") parseWindowsCurrentManifest(current.priorRaw);
  parseWindowsCurrentManifest(JSON.stringify(current.next));
  const snapshots = record(j.serviceSnapshot, "journal.serviceSnapshot");
  for (const role of SERVICE_ROLES) if (!snapshots[role] || record(snapshots[role], `journal snapshot ${role}`).role !== role) throw new Error(`journal snapshot missing ${role}`);
  const priorDefinitions = record(j.priorServiceDefinitions, "journal.priorServiceDefinitions");
  for (const role of SERVICE_ROLES) if (record(priorDefinitions[role], `journal prior definition ${role}`).role !== role) throw new Error(`journal prior definitions missing ${role}`);
  if (!Array.isArray(j.nextServiceDefinitions) || j.nextServiceDefinitions.length !== 4) throw new Error("journal requires four next definitions");
  const definitionRoles = new Set(j.nextServiceDefinitions.map((definition) => record(definition, "journal service definition").role));
  for (const role of SERVICE_ROLES) if (!definitionRoles.has(role)) throw new Error(`journal next definitions missing ${role}`);
  const running = record(j.runningBefore, "journal.runningBefore"); for (const role of SERVICE_ROLES) if (typeof running[role] !== "boolean") throw new Error(`invalid running vector ${role}`); if (running.updater) throw new Error("updater was already running");
  const healthBefore = record(j.healthBefore, "journal.healthBefore");
  for (const role of ["worker", "coordinator"] as const) {
    if (!(running[role] as boolean)) continue;
    const health = record(healthBefore[role], `journal healthBefore ${role}`);
    nonempty(health.version, `journal healthBefore ${role}.version`);
    nonempty(health.build, `journal healthBefore ${role}.build`);
    nonempty(health.processEpoch, `journal healthBefore ${role}.processEpoch`);
    if (role === "worker") httpsUrl(health.coordinatorUrl, "journal worker coordinatorUrl");
  }
  if (!Array.isArray(j.progress) || j.progress.length === 0 || j.progress.length > MAX_WINDOWS_UPDATE_PROGRESS) throw new Error("journal progress is invalid");
  let sequence = 0; for (const value of j.progress) { const p = record(value, "journal progress"); if (!Number.isSafeInteger(p.sequence) || (p.sequence as number) <= sequence || !isPhase(p.phase)) throw new Error("journal progress ordering is invalid"); sequence = p.sequence as number; nonempty(p.message, "progress message"); if (typeof p.terminal !== "boolean" || typeof p.success !== "boolean") throw new Error("progress flags are invalid"); }
}

export class DurableWindowsUpdateJournalStore implements WindowsUpdateJournalStore {
  constructor(readonly path: string = windowsUpdateJournalPath()) {}
  async load(): Promise<WindowsUpdateJournalV1 | null> {
    let raw: string;
    try { raw = await readFile(this.path, "utf8"); } catch (error) { if (nodeError(error)?.code === "ENOENT") return null; throw error; }
    const value = parseJson(raw, `Windows update journal at ${this.path}`); assertWindowsUpdateJournal(value); return value;
  }
  async save(journal: WindowsUpdateJournalV1): Promise<void> {
    assertWindowsUpdateJournal(journal);
    const parent = dirname(this.path); await mkdir(parent, { recursive: true }); await applyPrivateDacl(parent);
    const temporary = `${this.path}.${process.pid}.${randomUUID()}.tmp`;
    try { await durableWriteFile(temporary, `${JSON.stringify(journal)}\n`); await applyPrivateDacl(temporary); await durableReplace(temporary, this.path); await applyPrivateDacl(this.path); await flushDurablePath(parent); }
    finally { await durableRemove(temporary).catch(() => {}); }
  }
}

function parseJson(raw: string, label: string): unknown { try { return JSON.parse(raw); } catch (error) { throw new Error(`invalid ${label} JSON: ${String(error)}`); } }
function record(value: unknown, label: string): Record<string, unknown> { if (!value || typeof value !== "object" || Array.isArray(value)) throw new Error(`${label} must be an object`); return value as Record<string, unknown>; }
function nonempty(value: unknown, label: string): string { if (typeof value !== "string" || value.length === 0) throw new Error(`${label} must be non-empty`); return value; }
function sha(value: unknown, label: string): string { if (typeof value !== "string" || !SHA256_RE.test(value)) throw new Error(`${label} must be lowercase SHA-256`); return value; }
function normalizedSha(value: unknown, label: string): string { const normalized = typeof value === "string" ? value.trim().toLowerCase() : ""; if (!SHA256_RE.test(normalized)) throw new Error(`${label} must be SHA-256`); return normalized; }
function size(value: unknown, label: string): number { if (!Number.isSafeInteger(value) || (value as number) < 0) throw new Error(`${label} must be a non-negative safe integer`); return value as number; }
function httpsUrl(value: unknown, label: string): string { const text = nonempty(value, label); let url: URL; try { url = new URL(text); } catch { throw new Error(`${label} must be absolute`); } if (url.protocol !== "https:") throw new Error(`${label} must use HTTPS`); return text; }
function safeRelative(value: unknown, label: string): string { const path = nonempty(value, label).replaceAll("\\", "/"); if (isAbsolute(path) || path.startsWith("/") || path.includes("\0") || /^[A-Za-z]:/.test(path) || path.split("/").some((p) => !p || p === "." || p === "..")) throw new Error(`${label} is unsafe`); return path; }
function bounded(value: string): string { return (value.replace(/[\r\n]+/g, " ").trim() || "update progress").slice(0, 2048); }
function isState(value: unknown): value is WindowsUpdateState { return value === "forward" || value === "rolling-back" || value === "succeeded" || value === "rolled-back"; }
function isPhase(value: unknown): value is WindowsUpdatePhase { return ["prepared", "broker-started", "assets-staged", "services-stopped", "service-configs-switched", "current-manifest-switched", "services-restored", "health-proven", "committed", "rollback-started", "rollback-services-stopped", "rollback-configs-restored", "rollback-current-manifest-restored", "rollback-services-restored", "rolled-back"].includes(String(value)); }
function nodeError(error: unknown): NodeJS.ErrnoException | null { return error instanceof Error && "code" in error ? error as NodeJS.ErrnoException : null; }
