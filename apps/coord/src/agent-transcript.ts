// Durable agent-session transcript. The worker ring only coalesces live
// upserts; coord SQLite owns replay across worker and coordinator restarts.

import type { Database } from "bun:sqlite";
import { AgentEntry, type AgentEntry as AgentEntryValue } from "@roost/shared/wire/agent-entry";
import { log } from "@roost/shared/log";

const SWEEP_INTERVAL_MS = 24 * 60 * 60 * 1000;
const DAY_MS = 24 * 60 * 60 * 1000;
const BATCH_SIZE = 10_000;
export const AGENT_TRANSCRIPT_RETENTION_DAYS = 30;

interface AgentEntryRow {
  seq: number;
  ts: number;
  entry_json: string;
}

export function upsertAgentEntries(
  sqlite: Database,
  sessionId: string,
  entries: readonly AgentEntryValue[],
): void {
  if (entries.length === 0) return;
  const stmt = sqlite.prepare(
    `INSERT INTO agent_entries (session_id, seq, ts, entry_json)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(session_id, seq) DO UPDATE SET
       ts = excluded.ts,
       entry_json = excluded.entry_json`,
  );
  try {
    sqlite.transaction(() => {
      for (const entry of entries) {
        stmt.run(sessionId, entry.seq, entry.ts, JSON.stringify(entry));
      }
    })();
  } finally {
    stmt.finalize();
  }
}

export function pageAgentEntries(
  sqlite: Database,
  sessionId: string,
  beforeSeq: number,
  limit: number,
): { entries: AgentEntryValue[]; first_seq: number; more: boolean } {
  const rows = sqlite.query<AgentEntryRow, [string, number, number, number]>(
    `SELECT seq, ts, entry_json
       FROM agent_entries
      WHERE session_id = ? AND (? = 0 OR seq < ?)
      ORDER BY seq DESC
      LIMIT ?`,
  ).all(sessionId, beforeSeq, beforeSeq, limit + 1);
  const more = rows.length > limit;
  const pageRows = rows.slice(0, limit).reverse();
  const entries: AgentEntryValue[] = [];
  let warned = false;
  for (const row of pageRows) {
    let raw: unknown;
    try {
      raw = JSON.parse(row.entry_json);
    } catch {
      raw = null;
    }
    const parsed = AgentEntry.safeParse(raw);
    if (!parsed.success) {
      if (!warned) {
        warned = true;
        log.warn("agent-transcript", "entry_decode_failed", {
          session_id: sessionId,
          seq: row.seq,
          error: parsed.error.issues[0]?.message ?? "invalid",
        });
      }
      continue;
    }
    entries.push(parsed.data);
  }
  return {
    entries,
    first_seq: entries[0]?.seq ?? 0,
    more,
  };
}

export function nextAgentSeq(sqlite: Database, sessionId: string): number {
  const row = sqlite.query<{ next_seq: number }, [string]>(
    "SELECT COALESCE(MAX(seq), 0) + 1 AS next_seq FROM agent_entries WHERE session_id = ?",
  ).get(sessionId);
  return row?.next_seq ?? 1;
}

export function sweepAgentTranscripts(
  sqlite: Database,
  retentionDays: number,
): number {
  const cutoff = Date.now() - retentionDays * DAY_MS;
  // Closed rows are currently removed by main's startup janitor. Treat a
  // missing session as closed and use its final entry timestamp as the age;
  // open sessions are never eligible, regardless of transcript age.
  const stmt = sqlite.prepare(
    `DELETE FROM agent_entries
      WHERE (session_id, seq) IN (
        SELECT ae.session_id, ae.seq
          FROM agent_entries AS ae
          LEFT JOIN sessions AS s ON s.id = ae.session_id
         WHERE (
           (s.status = 'closed' AND s.closed_at < ?)
           OR (s.id IS NULL AND ae.ts < ?)
         )
         LIMIT ?
      )`,
  );
  let deleted = 0;
  try {
    for (;;) {
      const { changes } = stmt.run(cutoff, cutoff, BATCH_SIZE);
      deleted += changes;
      if (changes < BATCH_SIZE) break;
    }
  } finally {
    stmt.finalize();
  }
  return deleted;
}

function runSweep(sqlite: Database, retentionDays: number): void {
  try {
    const deleted = sweepAgentTranscripts(sqlite, retentionDays);
    if (deleted > 0) {
      log.info("agent-transcript", "transcripts_pruned", { deleted, retentionDays });
    }
  } catch (err) {
    log.error("agent-transcript", "transcript_prune_failed", {
      error: (err as Error).message,
    });
  }
}

export function scheduleAgentTranscriptRetention(
  sqlite: Database,
  retentionDays: number,
): void {
  runSweep(sqlite, retentionDays);
  setInterval(() => runSweep(sqlite, retentionDays), SWEEP_INTERVAL_MS).unref();
}
