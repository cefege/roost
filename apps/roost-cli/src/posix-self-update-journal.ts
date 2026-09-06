// Durable POSIX self-update transaction and executable rollback identity.
// The updater writes exact source/target hashes and lifecycle before rename.
// Recovery clears it only after worker, keeper, and executable proof.

import { existsSync, readFileSync } from "node:fs";
import { dirname, isAbsolute, join, normalize } from "node:path";
import { durableRemove, durableWriteFile } from "@roost/shared/durability";
import {
  JournaledKeeperUpdateV1Schema,
  type JournaledKeeperUpdateV1,
} from "@roost/shared/keeper-update";
import { roostServiceDir } from "@roost/shared/paths";
import { posixJournalObjectValue } from "./posix-deploy-journal.ts";

export type PosixSelfUpdatePhase =
  | "prepared"
  | "preparing_keeper"
  | "keeper_prepared"
  | "installed"
  | "rolling_back"
  | "committing";

export interface PosixSelfUpdateJournalV3 {
  schema_version: 3;
  phase: PosixSelfUpdatePhase;
  created_at_ms: number;
  target_version: string;
  executable_path: string;
  rollback_path: string;
  source_binary_sha256: string;
  target_binary_sha256: string;
  source_binary_mode: number;
  source_worker_sha: string;
  target_worker_sha: string;
  prior_lifecycle: "running";
  prior_startup_policy: "enabled";
  prior_coord_lifecycle: "running";
  prior_coord_startup_policy: "enabled";
  worker_fingerprint: string;
  keeper_update: JournaledKeeperUpdateV1;
}

export function posixSelfUpdateJournalPath(): string {
  return join(roostServiceDir(), "transactions", "self-update.json");
}

function canonicalExecutablePath(value: unknown, label: string): string {
  if (typeof value !== "string" || !isAbsolute(value) || normalize(value) !== value
    || /[\r\n\0]/.test(value)) throw new Error(`invalid self-update ${label}`);
  return value;
}

export function parsePosixSelfUpdateJournal(raw: string): PosixSelfUpdateJournalV3 {
  const value = posixJournalObjectValue(JSON.parse(raw), "self-update journal must be an object");
  const fields = [
    "schema_version", "phase", "created_at_ms", "target_version", "executable_path",
    "rollback_path", "source_binary_sha256", "target_binary_sha256", "source_binary_mode",
    "source_worker_sha", "target_worker_sha", "prior_lifecycle", "prior_startup_policy",
    "prior_coord_lifecycle", "prior_coord_startup_policy", "worker_fingerprint", "keeper_update",
  ];
  if (Object.keys(value).length !== fields.length
    || fields.some(field => !Object.hasOwn(value, field))) {
    throw new Error("self-update journal fields are incomplete or unexpected");
  }
  if (value.schema_version !== 3) throw new Error("unsupported self-update journal schema");
  const phases: PosixSelfUpdatePhase[] = [
    "prepared", "preparing_keeper", "keeper_prepared", "installed", "rolling_back", "committing",
  ];
  if (!phases.includes(value.phase as PosixSelfUpdatePhase)) {
    throw new Error("invalid self-update journal phase");
  }
  if (!Number.isSafeInteger(value.created_at_ms) || (value.created_at_ms as number) <= 0) {
    throw new Error("invalid self-update journal timestamp");
  }
  if (typeof value.target_version !== "string" || value.target_version.length < 1
    || value.target_version.length > 128 || /[\r\n\0]/.test(value.target_version)) {
    throw new Error("invalid self-update target version");
  }
  const executablePath = canonicalExecutablePath(value.executable_path, "executable path");
  const rollbackPath = canonicalExecutablePath(value.rollback_path, "rollback path");
  if (rollbackPath !== `${executablePath}.rollback` || dirname(rollbackPath) !== dirname(executablePath)) {
    throw new Error("invalid self-update rollback path");
  }
  for (const field of ["source_binary_sha256", "target_binary_sha256", "worker_fingerprint"] as const) {
    if (typeof value[field] !== "string" || !/^[0-9a-f]{64}$/.test(value[field])) {
      throw new Error(`invalid self-update ${field}`);
    }
  }
  for (const field of ["source_worker_sha", "target_worker_sha"] as const) {
    if (typeof value[field] !== "string" || !/^[0-9a-f]{40,64}$/i.test(value[field])) {
      throw new Error(`invalid self-update ${field}`);
    }
  }
  if (!Number.isInteger(value.source_binary_mode) || (value.source_binary_mode as number) < 0
    || (value.source_binary_mode as number) > 0o777) throw new Error("invalid source binary mode");
  if (value.prior_lifecycle !== "running"
    || value.prior_startup_policy !== "enabled") {
    throw new Error("self-update source must be running with automatic startup");
  }
  if (value.prior_coord_lifecycle !== "running"
    || value.prior_coord_startup_policy !== "enabled") {
    throw new Error("self-update coordinator must be running with automatic startup");
  }
  return {
    schema_version: 3,
    phase: value.phase as PosixSelfUpdatePhase,
    created_at_ms: value.created_at_ms as number,
    target_version: value.target_version,
    executable_path: executablePath,
    rollback_path: rollbackPath,
    source_binary_sha256: value.source_binary_sha256 as string,
    target_binary_sha256: value.target_binary_sha256 as string,
    source_binary_mode: value.source_binary_mode as number,
    source_worker_sha: value.source_worker_sha as string,
    target_worker_sha: value.target_worker_sha as string,
    prior_lifecycle: value.prior_lifecycle,
    prior_startup_policy: value.prior_startup_policy,
    prior_coord_lifecycle: value.prior_coord_lifecycle,
    prior_coord_startup_policy: value.prior_coord_startup_policy,
    worker_fingerprint: value.worker_fingerprint as string,
    keeper_update: JournaledKeeperUpdateV1Schema.parse(value.keeper_update),
  };
}

export function loadPosixSelfUpdateJournal(
  path = posixSelfUpdateJournalPath(),
): PosixSelfUpdateJournalV3 | null {
  return existsSync(path) ? parsePosixSelfUpdateJournal(readFileSync(path, "utf8")) : null;
}

export async function writePosixSelfUpdateJournal(
  journal: PosixSelfUpdateJournalV3,
  path = posixSelfUpdateJournalPath(),
): Promise<void> {
  const checked = parsePosixSelfUpdateJournal(JSON.stringify(journal));
  await durableWriteFile(path, `${JSON.stringify(checked)}\n`, { mode: 0o600 });
}

export async function checkpointPosixSelfUpdate(
  journal: PosixSelfUpdateJournalV3,
  phase: PosixSelfUpdatePhase,
  path = posixSelfUpdateJournalPath(),
): Promise<PosixSelfUpdateJournalV3> {
  const checkpoint = { ...journal, phase };
  await writePosixSelfUpdateJournal(checkpoint, path);
  return checkpoint;
}

export async function clearPosixSelfUpdateJournal(
  path = posixSelfUpdateJournalPath(),
): Promise<void> {
  await durableRemove(path);
}
