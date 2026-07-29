import { afterEach, describe, expect, test } from "bun:test";
import { Database } from "bun:sqlite";
import type { AgentEntry } from "@roost/shared/wire/agent-entry";
import {
  nextAgentSeq,
  pageAgentEntries,
  sweepAgentTranscripts,
  upsertAgentEntries,
} from "../src/agent-transcript.ts";

const BASE_TS = Date.UTC(2026, 0, 1);
const RETENTION_DAYS = 30;

let sqlite: Database | undefined;

afterEach(() => {
  sqlite?.close();
  sqlite = undefined;
});

function fixture(): Database {
  sqlite = new Database(":memory:");
  sqlite.exec(`
    CREATE TABLE sessions (
      id        TEXT PRIMARY KEY,
      status    TEXT NOT NULL,
      closed_at INTEGER
    );

    CREATE TABLE agent_entries (
      session_id TEXT    NOT NULL,
      seq        INTEGER NOT NULL,
      ts         INTEGER NOT NULL,
      entry_json TEXT    NOT NULL,
      PRIMARY KEY (session_id, seq)
    ) WITHOUT ROWID;
  `);
  return sqlite;
}

function assistant(seq: number, text = `assistant ${seq}`): AgentEntry {
  return {
    kind: "assistant",
    seq,
    ts: BASE_TS + seq,
    text,
    done: true,
  };
}

function rowCount(db: Database, sessionId: string): number {
  const row = db
    .prepare("SELECT count(*) AS count FROM agent_entries WHERE session_id = ?")
    .get(sessionId) as { count: number };
  return row.count;
}

describe("durable agent transcript", () => {
  test("upserts and pages entries by sequence without duplicating replacements", () => {
    const db = fixture();
    const sessionId = "agent-session";
    const entries = Array.from({ length: 300 }, (_, index) => assistant(index + 1));

    upsertAgentEntries(db, sessionId, entries);
    expect(nextAgentSeq(db, sessionId)).toBe(301);

    const newest = pageAgentEntries(db, sessionId, 0, 512);
    expect(newest.entries.map((entry) => entry.seq)).toEqual(
      Array.from({ length: 300 }, (_, index) => index + 1),
    );
    expect(newest.more).toBe(false);
    expect(newest.first_seq).toBe(1);

    const before100 = pageAgentEntries(db, sessionId, 100, 512);
    expect(before100.entries.map((entry) => entry.seq)).toEqual(
      Array.from({ length: 99 }, (_, index) => index + 1),
    );
    expect(before100.more).toBe(false);
    expect(before100.first_seq).toBe(1);

    const replacementText = "assistant 5 now contains the complete streamed response";
    upsertAgentEntries(db, sessionId, [assistant(5, replacementText)]);

    expect(rowCount(db, sessionId)).toBe(300);
    const replaced = pageAgentEntries(db, sessionId, 0, 512).entries.find(
      (entry) => entry.seq === 5,
    );
    expect(replaced).toEqual(assistant(5, replacementText));
  });

  test("skips corrupt persisted entries without failing the page", () => {
    const db = fixture();
    const sessionId = "agent-session";
    upsertAgentEntries(db, sessionId, [assistant(1), assistant(2)]);
    db.prepare(
      "INSERT INTO agent_entries (session_id, seq, ts, entry_json) VALUES (?, ?, ?, ?)",
    ).run(sessionId, 3, BASE_TS + 3, "{not valid json");

    let entries: AgentEntry[] = [];
    let firstSeq = -1;
    let more = true;
    expect(() => {
      const page = pageAgentEntries(db, sessionId, 0, 512);
      entries = page.entries;
      firstSeq = page.first_seq;
      more = page.more;
    }).not.toThrow();

    expect(entries).toEqual([assistant(1), assistant(2)]);
    expect(firstSeq).toBe(1);
    expect(more).toBe(false);
  });

  test("sweeps old closed-session transcripts and preserves open sessions", () => {
    const db = fixture();
    const closedSessionId = "closed-agent-session";
    const openSessionId = "open-agent-session";
    db.prepare("INSERT INTO sessions (id, status, closed_at) VALUES (?, ?, ?)").run(
      closedSessionId,
      "closed",
      0,
    );
    db.prepare("INSERT INTO sessions (id, status, closed_at) VALUES (?, ?, ?)").run(
      openSessionId,
      "open",
      null,
    );
    upsertAgentEntries(db, closedSessionId, [assistant(1), assistant(2)]);
    upsertAgentEntries(db, openSessionId, [assistant(1), assistant(2)]);

    const deleted = sweepAgentTranscripts(db, RETENTION_DAYS);

    expect(deleted).toBe(2);
    expect(pageAgentEntries(db, closedSessionId, 0, 512).entries).toEqual([]);
    expect(pageAgentEntries(db, openSessionId, 0, 512).entries).toEqual([
      assistant(1),
      assistant(2),
    ]);
  });
});
