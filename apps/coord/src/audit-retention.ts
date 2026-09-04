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

/** One day. The retention window is counted in these, this sweep runs once per
 *  one, and backup.ts schedules on the same value — one definition so the three
 *  cannot silently drift apart. */
export const DAY_MS = 24 * 60 * 60 * 1000;
// Rows per DELETE statement.
const BATCH_SIZE = 10_000;

// The ONLY methods this sweep is allowed to delete. Deliberately an explicit
// allowlist rather than a predicate or a blanket age cutoff: audit_log also
// holds the low-volume, high-forensic-value rows — PairApprove,
// AuthRedeemBrowser, WorkersDelete, WorkspacesDelete, SessionsKill,
// SessionsSpawn — and "when was this device authorised, and by whom" is exactly
// the question someone asks a year later. Those must survive regardless of age;
// deleting them reclaims kilobytes and costs the whole point of the table.
//
// To extend: add a method name here. Never add anything auth-, pair-, delete-
// or lifecycle-related, and never replace this with a wildcard.
//
// SessionsInput is the only entry, and the reason this module exists: ~42k rows
// of "who typed into which session", which is real audit data — it has to age
// out rather than be skipped at write time.
//
// UiReportState, SessionsGetScrollbackCells and TranscriptionGetConfig were
// briefly here too. They are non-mutating SPA polling, so they belong in
// AUDIT_SKIP_METHODS and are never written at all: sweeping them meant paying
// an INSERT per RPC to delete the row days later. Add future no-signal chatter
// there, not here.
//
// SessionsSpawn (1.2k) is deliberately absent from both: session lifecycle,
// low volume, worth keeping forever.
const AUDIT_SWEEP_METHODS: readonly string[] = [
  "SessionsInput",
];

export interface AuditSweepOptions {
  retentionDays: number;
  // Injected by tests so the window can be exercised without wall-clock waits.
  now?: number;
  batchSize?: number;
}
/** Removes the pre-hardening backlog of anonymous successful SPA/static reads.
 * Explicit API/export, internal lifecycle, WebSocket, and Connect paths are
 * excluded even if an old row happens to use GET or HEAD. */
export async function cleanupAnonymousStaticAuditLog(
  sqlite: Database,
  batchSize = BATCH_SIZE,
): Promise<number> {
  const stmt = sqlite.prepare(
    `DELETE FROM audit_log WHERE id IN (
       SELECT id FROM audit_log
       WHERE caller_fp IS NULL
         AND method IN ('GET', 'HEAD')
         AND status >= 200
         AND status < 400
         AND path <> '/api/db-export'
         AND path NOT LIKE '/api/%'
         AND path <> '/internal'
         AND path NOT LIKE '/internal/%'
         AND path <> '/ws'
         AND path NOT LIKE '/ws/%'
         AND path NOT LIKE '/roost.%'
       ORDER BY id
       LIMIT ?
     )`,
  );
  let deleted = 0;
  try {
    for (;;) {
      const { changes } = stmt.run(batchSize);
      deleted += changes;
      if (changes < batchSize) break;
      await Bun.sleep(0);
    }
  } finally {
    stmt.finalize();
  }
  return deleted;
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

async function runInitialCleanup(sqlite: Database): Promise<void> {
  try {
    const deleted = await cleanupAnonymousStaticAuditLog(sqlite);
    if (deleted > 0) {
      log.info("audit-retention", "anonymous_static_audit_cleaned", { deleted });
    }
  } catch (err) {
    log.error("audit-retention", "anonymous_static_audit_cleanup_failed", {
      error: (err as Error).message,
    });
  }
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

// The anonymous static backlog cleanup is deliberately startup-only. The
// recurring task retains the narrow forensic retention sweep.
export function scheduleAuditRetention(sqlite: Database, retentionDays: number): void {
  void (async () => {
    await runInitialCleanup(sqlite);
    await runSweep(sqlite, retentionDays);
  })();

  setInterval(() => {
    void runSweep(sqlite, retentionDays);
  }, DAY_MS).unref();
}
