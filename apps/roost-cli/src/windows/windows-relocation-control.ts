import { createHash } from "node:crypto";
import { readdir } from "node:fs/promises";
import { join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import { windowsConsumeUpdaterRequest, windowsCreateUpdaterRequest, windowsReadUpdaterArtifact } from "@roost/shared/windows-helper";
import type { WindowsRelocationBrokerCommand, WindowsRelocationOperationKind, WindowsRelocationResultFrame } from "@roost/shared/windows-relocation";
import { createWindowsServiceManager, type WindowsServiceManager } from "../service-ctl.ts";
import { createWindowsRelocationBrokerDeps, prepareWindowsRelocationJournal, rejectedWindowsRelocationJournal, validateWindowsRelocationCommand, type WindowsRelocationBrokerDeps } from "./windows-relocation-broker.ts";
import type { WindowsRelocationJournalStore, WindowsRelocationJournalV1 } from "./windows-relocation-journal.ts";
import { DurableWindowsUpdateJournalStore, type WindowsUpdateJournalStore } from "./windows-update-journal.ts";

const MAX_REQUESTS = 16;
const MAX_REQUEST_BYTES = 32 * 1024;
interface AdmissionRecord { schemaVersion: 1; kind: "relocation"; command: WindowsRelocationBrokerCommand }

export interface WindowsRelocationControlDeps {
  store: WindowsRelocationJournalStore;
  updateStore: WindowsUpdateJournalStore;
  services: WindowsServiceManager;
  broker: WindowsRelocationBrokerDeps;
  requestDir: string;
  platform?: NodeJS.Platform;
  now?: () => number;
  sleep?: (milliseconds: number) => Promise<void>;
}
export interface ExecuteWindowsRelocationOptions { beforeUpdaterStart: () => Promise<void> }

/** Worker queue/status path: only exact native inbox creation and read-only result access. */
export async function executeWindowsRelocationBrokerCommand(
  command: WindowsRelocationBrokerCommand,
  options: ExecuteWindowsRelocationOptions,
  deps?: WindowsRelocationControlDeps,
): Promise<WindowsRelocationResultFrame> {
  validateWindowsRelocationCommand(command);
  deps ??= defaultDeps(command.operationKind);
  if ((deps.platform ?? process.platform) !== "win32") throw new Error("Windows relocation refused off Windows");
  const current = await deps.store.load();
  if (command.action === "STATUS" && current && (current.relocationId !== command.relocationId
    || current.handoffId !== command.handoffId || current.operationKind !== command.operationKind)) return frame(command, null);
  assertTransition(command, current);
  if (command.action === "STATUS") {
    if (!current) return frame(command, null);
    if (needsBroker(current)) {
      if ((await deps.services.query("updater")).state !== "running") {
        await options.beforeUpdaterStart();
        await deps.services.start("updater");
      }
      return await awaitSettledStatus(command, deps);
    }
    return frame(command, current);
  }
  const contents = new TextEncoder().encode(`${JSON.stringify({ schemaVersion: 1, kind: "relocation", command } satisfies AdmissionRecord)}\n`);
  if (contents.byteLength > MAX_REQUEST_BYTES) throw new Error("relocation request exceeds 32KiB");
  const digest = createHash("sha256").update(`${command.relocationId}:${command.action}`).digest("hex");
  await windowsCreateUpdaterRequest(join(deps.requestDir, `relocation-${digest}.json`), contents);
  const revision = current?.relocationId === command.relocationId ? current.revision : 0;
  await options.beforeUpdaterStart();
  await deps.services.start("updater");
  return await awaitResult(command, revision, deps);
}

/** Updater-only admission; the broker, not this function, owns the machine lease. */
export async function admitPendingWindowsRelocationRequest(deps?: WindowsRelocationControlDeps): Promise<WindowsRelocationJournalV1 | null> {
  const bootstrap = deps ?? defaultDeps("worker-endpoint");
  if ((bootstrap.platform ?? process.platform) !== "win32") throw new Error("Windows relocation admission refused off Windows");
  await bootstrap.broker.native.assertUpdaterServiceContext();
  const pending = await readPending(bootstrap.requestDir);
  if (!pending) return null;
  deps ??= defaultDeps(pending.record.command.operationKind);
  const update = await deps.updateStore.load();
  if (update && (update.state === "forward" || update.state === "rolling-back")) {
    throw new Error(`Windows update transaction ${update.jobId} is already active`);
  }
  const command = pending.record.command;
  let existing = await deps.store.load();
  if (existing?.admissionPath === pending.path && existing.relocationId === command.relocationId
    && existing.handoffId === command.handoffId && existing.operationKind === command.operationKind) {
    await windowsConsumeUpdaterRequest(pending.path);
    existing = { ...existing, admissionPath: undefined, updatedAt: new Date(deps.now?.() ?? Date.now()).toISOString() };
    await deps.store.save(existing);
    return existing;
  }
  assertTransition(command, existing);
  let journal: WindowsRelocationJournalV1;
  if (command.action === "START") {
    try { journal = await prepareWindowsRelocationJournal(command, pending.path, deps.broker); }
    catch (error) { journal = rejectedWindowsRelocationJournal(command, pending.path, error, deps.broker); }
  } else {
    journal = {
      ...existing!,
      phase: command.action === "APPLY" ? "apply-requested" : command.action === "COMMIT" ? "commit-requested" : "restore-requested",
      revision: existing!.revision + 1,
      pendingAction: command.action as "APPLY" | "COMMIT" | "RESTORE",
      admissionPath: pending.path,
      result: undefined,
      updatedAt: new Date(deps.now?.() ?? Date.now()).toISOString(),
    };
  }
  await deps.store.save(journal);
  await windowsConsumeUpdaterRequest(pending.path);
  journal = { ...journal, admissionPath: undefined, updatedAt: new Date(deps.now?.() ?? Date.now()).toISOString() };
  await deps.store.save(journal);
  return journal;
}

function assertTransition(command: WindowsRelocationBrokerCommand, journal: WindowsRelocationJournalV1 | null): void {
  if (command.action === "START") {
    if (journal?.relocationId === command.relocationId) throw new Error("replayed relocation START is forbidden; use STATUS");
    if (journal && !["committed", "rolled-back"].includes(journal.phase)) throw new Error(`relocation ${journal.relocationId} is active`);
    return;
  }
  if (!journal) {
    if (command.action === "STATUS") return;
    throw new Error("relocation is not prepared");
  }
  if (journal.relocationId !== command.relocationId || journal.handoffId !== command.handoffId
    || journal.operationKind !== command.operationKind) throw new Error("relocation identity does not match durable journal");
  if (command.action === "STATUS") return;
  const allowed = command.action === "APPLY" ? journal.phase === "prepared"
    : command.action === "COMMIT" ? journal.phase === "applied" : journal.phase === "prepared" || journal.phase === "applied";
  if (!allowed) throw new Error(`replayed or invalid relocation ${command.action} in phase ${journal.phase}`);
}

async function readPending(requestDir: string): Promise<{ path: string; record: AdmissionRecord } | null> {
  const entries = await readdir(requestDir, { withFileTypes: true }).catch((error) => {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return [];
    throw error;
  });
  const candidates = entries.filter((entry) => entry.name.startsWith("relocation-"));
  if (candidates.length > MAX_REQUESTS) throw new Error("Windows relocation inbox exceeds its bounded limit");
  let firstError: unknown;
  for (const entry of candidates.sort((left, right) => left.name.localeCompare(right.name))) {
    const path = join(requestDir, entry.name);
    try {
      const record = JSON.parse(new TextDecoder().decode(await windowsReadUpdaterArtifact(path, "private", MAX_REQUEST_BYTES))) as AdmissionRecord;
      if (record.schemaVersion !== 1 || record.kind !== "relocation" || !record.command) throw new Error("unsupported request schema");
      validateWindowsRelocationCommand(record.command);
      const digest = createHash("sha256").update(`${record.command.relocationId}:${record.command.action}`).digest("hex");
      if (join(requestDir, `relocation-${digest}.json`).toLowerCase() !== path.toLowerCase()) throw new Error("request filename mismatch");
      return { path, record };
    } catch (error) { firstError ??= error; }
  }
  if (firstError) throw new Error(`Windows relocation inbox contains no valid request: ${String(firstError)}`);
  return null;
}

async function awaitResult(command: WindowsRelocationBrokerCommand, priorRevision: number, deps: WindowsRelocationControlDeps): Promise<WindowsRelocationResultFrame> {
  const deadline = (deps.now?.() ?? Date.now()) + 60_000;
  for (;;) {
    const journal = await deps.store.load();
    if (journal && journal.relocationId === command.relocationId && journal.result?.action === command.action
      && journal.result.revision > priorRevision) return frame(command, journal);
    if ((deps.now?.() ?? Date.now()) >= deadline) throw new Error(`timed out awaiting durable relocation ${command.action} result`);
    if ((await deps.services.query("updater")).state === "stopped") await deps.services.start("updater");
    await (deps.sleep ?? Bun.sleep)(100);
  }
}

async function awaitSettledStatus(command: WindowsRelocationBrokerCommand, deps: WindowsRelocationControlDeps): Promise<WindowsRelocationResultFrame> {
  const deadline = (deps.now?.() ?? Date.now()) + 60_000;
  for (;;) {
    const journal = await deps.store.load();
    if (journal && journal.relocationId === command.relocationId && !needsBroker(journal)) return frame(command, journal);
    if ((deps.now?.() ?? Date.now()) >= deadline) throw new Error("timed out awaiting durable relocation recovery");
    if ((await deps.services.query("updater")).state === "stopped") await deps.services.start("updater");
    await (deps.sleep ?? Bun.sleep)(100);
  }
}

function frame(command: WindowsRelocationBrokerCommand, journal: WindowsRelocationJournalV1 | null): WindowsRelocationResultFrame {
  return { requestId: command.requestId, relocationId: command.relocationId, handoffId: command.handoffId,
    operationKind: command.operationKind, revision: journal?.revision ?? 0, phase: journal?.phase ?? "missing",
    message: journal?.result?.message ?? "relocation result is not available", terminal: journal?.result !== undefined,
    success: journal?.result?.success ?? false, error: journal?.result?.error ?? "" };
}
function needsBroker(journal: WindowsRelocationJournalV1): boolean {
  return ["apply-requested", "applying", "commit-requested", "restore-requested", "restoring"].includes(journal.phase);
}
function defaultDeps(operationKind: WindowsRelocationOperationKind): WindowsRelocationControlDeps {
  const serviceDir = roostServiceDir();
  const broker = createWindowsRelocationBrokerDeps(operationKind);
  return { store: broker.store, updateStore: new DurableWindowsUpdateJournalStore(), services: createWindowsServiceManager(),
    broker, requestDir: join(serviceDir, "requests") };
}
