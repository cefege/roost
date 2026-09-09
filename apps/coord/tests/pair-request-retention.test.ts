import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { Database } from "bun:sqlite";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  PAIR_REQUEST_TOMBSTONE_MS,
  sweepPairRequests,
} from "../src/pair-request-retention.ts";

const NOW = Date.UTC(2026, 6, 28);
const workdirs: string[] = [];
const closers: Array<() => Promise<void>> = [];

afterEach(async () => {
  for (const close of closers.splice(0)) await close();
  for (const directory of workdirs.splice(0)) {
    rmSync(directory, { recursive: true, force: true });
  }
});

async function fixture(): Promise<Database> {
  const directory = mkdtempSync(join(tmpdir(), "roost-pair-retention-"));
  workdirs.push(directory);
  const opened = openDb(join(directory, "coord.db"));
  await runMigrations(opened.sqlite);
  closers.push(opened.close);
  return opened.sqlite;
}

function seedPairRequest(
  sqlite: Database,
  row: {
    id: string;
    status: string;
    expiresAtMs: number;
    decidedAtMs: number | null;
  },
): void {
  sqlite.query(`
    INSERT INTO pair_requests
      (id, ephemeral_id, public_key, label, status, created_at_ms, decided_at_ms, expires_at_ms)
    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
  `).run(
    row.id,
    row.id,
    new Uint8Array(32),
    row.id,
    row.status,
    NOW - 1_000,
    row.decidedAtMs,
    row.expiresAtMs,
  );
}

describe("pair-request retention sweep", () => {
  test("expires overdue pending rows and deletes only old terminal tombstones", async () => {
    const sqlite = await fixture();
    seedPairRequest(sqlite, {
      id: "pending-expired",
      status: "pending",
      expiresAtMs: NOW - 1,
      decidedAtMs: null,
    });
    seedPairRequest(sqlite, {
      id: "pending-fresh",
      status: "pending",
      expiresAtMs: NOW + 60_000,
      decidedAtMs: null,
    });
    seedPairRequest(sqlite, {
      id: "approved-old",
      status: "approved",
      expiresAtMs: NOW - 2_000,
      decidedAtMs: NOW - PAIR_REQUEST_TOMBSTONE_MS - 1,
    });
    seedPairRequest(sqlite, {
      id: "denied-at-cutoff",
      status: "denied",
      expiresAtMs: NOW - 2_000,
      decidedAtMs: NOW - PAIR_REQUEST_TOMBSTONE_MS,
    });
    seedPairRequest(sqlite, {
      id: "expired-recent",
      status: "expired",
      expiresAtMs: NOW - 2_000,
      decidedAtMs: NOW - 1,
    });

    const result = sweepPairRequests(sqlite, NOW);

    expect(result).toEqual({ expired: ["pending-expired"], deleted: 2 });
    const rows = sqlite.query(`
      SELECT ephemeral_id, status, decided_at_ms
      FROM pair_requests
      ORDER BY ephemeral_id
    `).all() as Array<{ ephemeral_id: string; status: string; decided_at_ms: number | null }>;
    expect(rows).toEqual([
      { ephemeral_id: "expired-recent", status: "expired", decided_at_ms: NOW - 1 },
      { ephemeral_id: "pending-expired", status: "expired", decided_at_ms: NOW },
      { ephemeral_id: "pending-fresh", status: "pending", decided_at_ms: null },
    ]);
  });

  test("does not delete a pending request even when its decision timestamp is old", async () => {
    const sqlite = await fixture();
    seedPairRequest(sqlite, {
      id: "pending-with-old-decision",
      status: "pending",
      expiresAtMs: NOW + 60_000,
      decidedAtMs: NOW - PAIR_REQUEST_TOMBSTONE_MS - 1,
    });

    const result = sweepPairRequests(sqlite, NOW);

    expect(result).toEqual({ expired: [], deleted: 0 });
    expect(sqlite.query(
      "SELECT status FROM pair_requests WHERE ephemeral_id = ?",
    ).get("pending-with-old-decision")).toEqual({ status: "pending" });
  });
});
