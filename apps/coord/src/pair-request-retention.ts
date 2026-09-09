// Pair-request retention owns expiry and tombstone cleanup for pairing rows.
// Its sweep accepts a timestamp so callers can exercise the ten-minute lifetime.
// The scheduler publishes expired IDs to pairBus and records non-empty runs.

import type { Database } from "bun:sqlite";
import { log } from "@roost/shared/log";
import { DAY_MS } from "./audit-retention.ts";
import { pairBus } from "./buses.ts";

export const PAIR_REQUEST_SWEEP_INTERVAL_MS = 60_000;
export const PAIR_REQUEST_TOMBSTONE_MS = DAY_MS;

const BATCH_SIZE = 1_000;

type ExpiredPairRequestRow = {
  ephemeral_id: string;
};

/**
 * Expires overdue pending requests and removes old terminal tombstones.
 * Each write is bounded so a backlog cannot become one uninterruptible delete.
 */
export function sweepPairRequests(
  sqlite: Database,
  now: number,
): { expired: string[]; deleted: number } {
  const expired: string[] = [];
  const expireStatement = sqlite.prepare(`
    UPDATE pair_requests
    SET status = 'expired', decided_at_ms = ?
    WHERE id IN (
      SELECT id
      FROM pair_requests
      WHERE status = 'pending' AND expires_at_ms <= ?
      ORDER BY expires_at_ms
      LIMIT ?
    )
    RETURNING ephemeral_id
  `);
  const tombstoneStatement = sqlite.prepare(`
    DELETE FROM pair_requests
    WHERE id IN (
      SELECT id
      FROM pair_requests
      WHERE status <> 'pending'
        AND decided_at_ms <= ?
      ORDER BY decided_at_ms
      LIMIT ?
    )
  `);

  try {
    for (;;) {
      const rows = expireStatement.all(now, now, BATCH_SIZE) as ExpiredPairRequestRow[];
      for (const row of rows) expired.push(row.ephemeral_id);
      if (rows.length < BATCH_SIZE) break;
    }

    const tombstoneCutoff = now - PAIR_REQUEST_TOMBSTONE_MS;
    let deleted = 0;
    for (;;) {
      const result = tombstoneStatement.run(tombstoneCutoff, BATCH_SIZE);
      deleted += result.changes;
      if (result.changes < BATCH_SIZE) return { expired, deleted };
    }
  } finally {
    expireStatement.finalize();
    tombstoneStatement.finalize();
  }
}

function runScheduledPairRequestSweep(sqlite: Database): void {
  try {
    const result = sweepPairRequests(sqlite, Date.now());
    for (const ephemeral_id of result.expired) {
      pairBus.publish({ kind: "removed", ephemeral_id });
    }
    if (result.expired.length > 0 || result.deleted > 0) {
      log.info("pair-retention", "swept", {
        expired: result.expired.length,
        deleted: result.deleted,
      });
    }
  } catch (err) {
    log.error("pair-retention", "sweep_failed", { error: (err as Error).message });
  }
}

export function schedulePairRequestRetention(sqlite: Database): void {
  runScheduledPairRequestSweep(sqlite);
  setInterval(() => runScheduledPairRequestSweep(sqlite), PAIR_REQUEST_SWEEP_INTERVAL_MS).unref();
}
