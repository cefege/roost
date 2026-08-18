import { join } from "node:path";
import { roostServiceDir } from "@roost/shared/paths";
import {
  windowsReadUpdaterArtifact,
  windowsReplaceUpdaterArtifact,
} from "@roost/shared/windows-helper";
import {
  WINDOWS_RELOCATION_SCHEMA_VERSION,
  type WindowsRelocationCommandAction,
  type WindowsRelocationOperation,
  type WindowsRelocationOperationKind,
  type WindowsRelocationRoleOverride,
} from "@roost/shared/windows-relocation";

const MAX_RELOCATION_JOURNAL_BYTES = 128 * 1024;

export type WindowsRelocationPhase =
  | "prepared"
  | "apply-requested"
  | "applying"
  | "applied"
  | "commit-requested"
  | "committed"
  | "restore-requested"
  | "restoring"
  | "rolled-back";

export interface WindowsRelocationTerminalResult {
  action: Exclude<WindowsRelocationCommandAction, "STATUS">;
  revision: number;
  success: boolean;
  message: string;
  error?: string;
  completedAt: string;
}

export interface WindowsCoordinatorRelocationCheckpoint {
  phase:
    | "captured"
    | "coordinator-stopped"
    | "state-promoted"
    | "override-applied"
    | "route-applied"
    | "coordinator-started"
    | "healthy"
    | "restored"
    | "committed";
  priorCoordinatorRunning: boolean;
  priorTailscaleConfig: string;
  rollbackPrepared: boolean;
}

export interface WindowsRelocationJournalV1 {
  schemaVersion: typeof WINDOWS_RELOCATION_SCHEMA_VERSION;
  relocationId: string;
  handoffId: string;
  operationKind: WindowsRelocationOperationKind;
  operation: WindowsRelocationOperation;
  phase: WindowsRelocationPhase;
  revision: number;
  priorOverrideRaw: string | null;
  desiredOverride: WindowsRelocationRoleOverride;
  coordinator?: WindowsCoordinatorRelocationCheckpoint;
  pendingAction?: Exclude<WindowsRelocationCommandAction, "START" | "STATUS">;
  admissionPath?: string;
  result?: WindowsRelocationTerminalResult;
  createdAt: string;
  updatedAt: string;
}

export interface WindowsRelocationJournalStore {
  readonly path: string;
  load(): Promise<WindowsRelocationJournalV1 | null>;
  save(journal: WindowsRelocationJournalV1): Promise<void>;
}

export function windowsRelocationJournalPath(
  serviceDir: string = roostServiceDir(),
  operationKind: WindowsRelocationOperationKind = "worker-endpoint",
): string {
  const role = operationKind === "worker-endpoint" ? "worker" : "coordinator";
  return join(serviceDir, "data", "updater", `relocation-${role}-v1.json`);
}

export class DurableWindowsRelocationJournalStore implements WindowsRelocationJournalStore {
  readonly path: string;

  constructor(path: string = windowsRelocationJournalPath()) {
    this.path = path;
  }

  async load(): Promise<WindowsRelocationJournalV1 | null> {
    let contents: Uint8Array;
    try {
      contents = await windowsReadUpdaterArtifact(
        this.path,
        "control",
        MAX_RELOCATION_JOURNAL_BYTES,
      );
    } catch (error) {
      if (/\[win32=(?:2|3)\]/.test(String(error))) return null;
      throw error;
    }
    if (contents.byteLength === 0) throw new Error("Windows relocation journal is empty");
    const parsed = JSON.parse(new TextDecoder().decode(contents)) as unknown;
    assertWindowsRelocationJournal(parsed);
    return parsed;
  }

  async save(journal: WindowsRelocationJournalV1): Promise<void> {
    assertWindowsRelocationJournal(journal);
    const contents = new TextEncoder().encode(`${JSON.stringify(journal)}\n`);
    if (contents.byteLength > MAX_RELOCATION_JOURNAL_BYTES) {
      throw new Error("Windows relocation journal exceeds its bounded limit");
    }
    await windowsReplaceUpdaterArtifact(this.path, "control", contents);
  }
}

export function assertWindowsRelocationJournal(
  value: unknown,
): asserts value is WindowsRelocationJournalV1 {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new Error("Windows relocation journal must be an object");
  }
  const journal = value as Partial<WindowsRelocationJournalV1>;
  if (
    journal.schemaVersion !== WINDOWS_RELOCATION_SCHEMA_VERSION
    || typeof journal.relocationId !== "string"
    || typeof journal.handoffId !== "string"
    || (journal.operationKind !== "worker-endpoint" && journal.operationKind !== "coordinator-promotion")
    || !journal.operation
    || journal.operation.kind !== journal.operationKind
    || journal.operation.relocationId !== journal.relocationId
    || journal.operation.handoffId !== journal.handoffId
    || !isPhase(journal.phase)
    || !Number.isSafeInteger(journal.revision)
    || (journal.revision as number) < 1
    || (journal.priorOverrideRaw !== null && typeof journal.priorOverrideRaw !== "string")
    || !journal.desiredOverride
    || journal.desiredOverride.relocationId !== journal.relocationId
    || journal.desiredOverride.handoffId !== journal.handoffId
    || typeof journal.createdAt !== "string"
    || typeof journal.updatedAt !== "string"
  ) {
    throw new Error("invalid Windows relocation journal");
  }
  if (journal.pendingAction !== undefined && !["APPLY", "COMMIT", "RESTORE"].includes(journal.pendingAction)) {
    throw new Error("invalid Windows relocation pending action");
  }
  if (journal.admissionPath !== undefined && typeof journal.admissionPath !== "string") {
    throw new Error("invalid Windows relocation admission path");
  }
  if (journal.operationKind === "coordinator-promotion" && journal.coordinator !== undefined) {
    const checkpoint = journal.coordinator;
    if (
      ![
        "captured", "coordinator-stopped", "state-promoted", "override-applied",
        "route-applied", "coordinator-started", "healthy", "restored", "committed",
      ].includes(checkpoint.phase)
      || typeof checkpoint.priorCoordinatorRunning !== "boolean"
      || typeof checkpoint.priorTailscaleConfig !== "string"
      || typeof checkpoint.rollbackPrepared !== "boolean"
    ) throw new Error("invalid Windows coordinator relocation checkpoint");
  } else if (journal.operationKind !== "coordinator-promotion" && journal.coordinator !== undefined) {
    throw new Error("worker endpoint relocation contains coordinator state");
  }
  if (journal.result !== undefined) {
    const result = journal.result;
    if (
      !["START", "APPLY", "COMMIT", "RESTORE"].includes(result.action)
      || !Number.isSafeInteger(result.revision)
      || typeof result.success !== "boolean"
      || typeof result.message !== "string"
      || (result.error !== undefined && typeof result.error !== "string")
      || typeof result.completedAt !== "string"
    ) {
      throw new Error("invalid Windows relocation terminal result");
    }
  }
}

function isPhase(value: unknown): value is WindowsRelocationPhase {
  return [
    "prepared",
    "apply-requested",
    "applying",
    "applied",
    "commit-requested",
    "committed",
    "restore-requested",
    "restoring",
    "rolled-back",
  ].includes(value as WindowsRelocationPhase);
}
