// audit_log retention sweep. Ages out the high-volume/low-signal RPC rows and
// leaves everything else alone. Callers: main.ts (startup + setInterval 24h).
//
// Why: audit_log had no retention at all and reached 7,026,358 rows / 1.0 GB
// before a one-off manual prune cut it back to ~174k. Without a sweep it just
// regrows. AUDIT_SKIP_METHODS (connect/auth-interceptor.ts) stopped the
// pure-noise writers, but the remaining top contributor — SessionsInput, i.e.
// who typed into which session — is genuine audit data. It has to be aged out,
// not skipped.

import type { Database } from "bun:sqlite";
import { log } from "@roost/shared/log";

const SWEEP_INTERVAL_MS = 24 * 60 * 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;
// Rows per DELETE statement.
const BATCH_SIZE = 10_000;

// The ONLY methods this sweep is allowed to delete. Deliberately an explicit
// allowlist rather than a predicate or a blanket age cutoff: audit_log also
// holds the low-volume, high-forensic-value rows — PairApprove,
// AuthAuthorizeBrowser, WorkersDelete, WorkspacesDelete, SessionsKill,
// SessionsSpawn — and "when was this device authorised, and by whom" is exactly
// the question someone asks a year later. Those must survive regardless of age;
// deleting them reclaims kilobytes and costs the whole point of the table.
//
// To extend: add a method name here. Never add anything auth-, pair-, delete-
// or lifecycle-related, and never replace this with a wildcard.
//
// Current entries, with their share of the live table at the time of writing:
//   SessionsInput               41.8k — keystrokes; genuine audit, but the
//                                       volume driver and the reason this exists
//   UiReportState               40.3k — SPA reporting its own view state
//   SessionsGetScrollbackCells  32.3k — SPA polling terminal render state
//   TranscriptionGetConfig      27.4k — SPA polling a config read
// The last three are non-mutating chatter. SessionsSpawn (1.2k) is deliberately
// absent: it is session lifecycle, low volume, and worth keeping forever.
const AUDIT_SWEEP_METHODS: readonly string[] = [
  "SessionsInput",
  "UiReportState",
  "SessionsGetScrollbackCells",
  "TranscriptionGetConfig",
];

export interface AuditSweepOptions {
  retentionDays: number;
  // Injected by tests so the window can be exercised without wall-clock waits.
  now?: number;
  batchSize?: number;
}

// Deletes swept-method audit_log rows older than the window. Returns the count.
export async function sweepAuditLog(sqlite: Database, opts: AuditSweepOptions): Promise<number> {
  const now = opts.now ?? Date.now();
  const batchSize = opts.batchSize ?? BATCH_SIZE;
  // ts is epoch MILLISECONDS (see migrations/0001_init.sql).
  const cutoff = now - opts.retentionDays * DAY_MS;

  // path is `/<service>/<Method>` (auth-interceptor.ts:108) and the service
  // prefix varies across proto packages, so match the trailing segment. The
  // `method` column is not usable here — the interceptor writes the literal
  // "POST" into it.
  const pathFilter = AUDIT_SWEEP_METHODS.map(() => "path LIKE ?").join(" OR ");
  const pathParams = AUDIT_SWEEP_METHODS.map((m) => `%/${m}`);

  // Bounded batches, not one unbounded DELETE: a first run against a large
  // backlog must not hold the write lock for its whole duration on a live
  // coordinator. bun:sqlite is not built with SQLITE_ENABLE_UPDATE_DELETE_LIMIT,
  // so the LIMIT rides on a subselect. audit_log_ts turns `ts < ?` into a
  // bounded index range scan; the path check is a per-row filter on top.
  const stmt = sqlite.prepare(
    `DELETE FROM audit_log WHERE id IN (
       SELECT id FROM audit_log WHERE ts < ? AND (${pathFilter}) ORDER BY ts LIMIT ?
     )`,
  );
  let deleted = 0;
  try {
    for (;;) {
      const { changes } = stmt.run(cutoff, ...pathParams, batchSize);
      deleted += changes;
      // A short batch means the cutoff range is exhausted.
      if (changes < batchSize) break;
      // bun:sqlite is synchronous and the interceptor's audit inserts run on
      // this same thread, so a tight loop over a multi-million-row backlog
      // would block every RPC until it finished — the batching alone buys
      // nothing without an explicit yield between statements.
      await Bun.sleep(0);
    }
  } finally {
    stmt.finalize();
  }

  // Deliberately no VACUUM here. Reclaiming the freed pages needs an EXCLUSIVE
  // lock over the whole file and rewrites it end to end — on a live coord that
  // stalls every RPC for as long as it takes. The freed pages are reused by
  // subsequent inserts, so the file stops growing even without one. Shrinking
  // it on disk is a manual, out-of-hours operation.
  return deleted;
}

async function runSweep(sqlite: Database, retentionDays: number): Promise<void> {
  try {
    const deleted = await sweepAuditLog(sqlite, { retentionDays });
    // Silence is the steady state; only a real deletion is worth a line.
    if (deleted > 0) {
      log.info("audit-retention", "audit_log_pruned", { deleted, retentionDays });
    }
  } catch (err) {
    log.error("audit-retention", "audit_log_prune_failed", { error: (err as Error).message });
  }
}

// Schedules a 24h recurring sweep. Runs one immediately at startup: unlike
// backups there is no on-disk staleness marker to check, and the sweep itself
// is the cheap check — an index range scan that deletes nothing and logs
// nothing when there is nothing to age out.
export function scheduleAuditRetention(sqlite: Database, retentionDays: number): void {
  void runSweep(sqlite, retentionDays);

  setInterval(() => {
    void runSweep(sqlite, retentionDays);
  }, SWEEP_INTERVAL_MS).unref();
}
