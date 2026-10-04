// Read-only probe of the coordinator database the stack launched: the live
// worker rows with the keeper runtime each worker last reported. Called by
// terminal-local-fast-path-helpers.ts to compare keeper identity across a
// coordinator bounce; depends on bun:sqlite and the keeper observation schema.

import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";
import {
  KeeperRuntimeObservationV1Schema,
  type KeeperRuntimeObservationV1,
} from "./keeper-update.ts";

export interface CoordinatorWorkerRow {
  fingerprint: string;
  label: string;
  /** Null when the worker never reported one or the stored JSON does not parse. */
  keeperRuntime: KeeperRuntimeObservationV1 | null;
  lastSeenMs: number;
}

/** Every non-deleted worker row, read in one transaction. Throws when the
 *  database does not exist rather than reporting an empty roster. */
export function coordinatorWorkerRows(databasePath: string): CoordinatorWorkerRow[] {
  if (!existsSync(databasePath)) {
    throw new Error(`coordinator database not found: ${databasePath}`);
  }
  const db = new Database(databasePath, { readonly: true });
  try {
    const rows = db.query(
      `SELECT fp, label, keeper_runtime_json, last_seen_ms
       FROM workers
       WHERE deleted_at_ms IS NULL`,
    ).all() as Array<{ fp: string; label: string; keeper_runtime_json: string | null; last_seen_ms: number }>;
    return rows.map((row) => ({
      fingerprint: row.fp,
      label: row.label,
      keeperRuntime: parseKeeperRuntime(row.keeper_runtime_json),
      lastSeenMs: row.last_seen_ms,
    }));
  } finally {
    db.close();
  }
}

function parseKeeperRuntime(serialized: string | null): KeeperRuntimeObservationV1 | null {
  if (!serialized) return null;
  try {
    const parsed = KeeperRuntimeObservationV1Schema.safeParse(JSON.parse(serialized));
    return parsed.success ? parsed.data : null;
  } catch {
    return null;
  }
}
