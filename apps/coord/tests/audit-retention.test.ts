import { afterEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import type { Database } from "bun:sqlite";
import { openDb } from "../src/db/connection.ts";
import { runMigrations } from "../src/db/migrate.ts";
import {
  cleanupAnonymousStaticAuditLog,
  sweepAuditLog,
} from "../src/audit-retention.ts";

const DAY_MS = 24 * 60 * 60 * 1000;
// Fixed clock — the window is driven by the injected `now`, never by wall time.
const NOW = Date.UTC(2026, 6, 28);
const SVC = "/roost.v1.CoordinatorService";

const workdirs: string[] = [];
const closers: (() => Promise<void>)[] = [];
afterEach(async () => {
  for (const close of closers.splice(0)) await close();
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

async function fixture(): Promise<Database> {
  const dir = mkdtempSync(join(tmpdir(), "roost-audit-retention-"));
  workdirs.push(dir);
  const opened = openDb(join(dir, "coord.db"));
  await runMigrations(opened.sqlite);
  closers.push(() => opened.close());
  return opened.sqlite;
}

function seed(sqlite: Database, rows: { ts: number; path: string }[]): void {
  const insert = sqlite.query(
    "INSERT INTO audit_log (ts, caller_fp, method, path, status) VALUES (?, 'fp', 'POST', ?, 200)",
  );
  sqlite.exec("BEGIN");
  for (const row of rows) insert.run(row.ts, row.path);
  sqlite.exec("COMMIT");
}

function pathsLeft(sqlite: Database): string[] {
  // bun:sqlite .all() is untyped; this query literally selects `path`.
  const rows = sqlite.query("SELECT path FROM audit_log ORDER BY id").all() as { path: string }[];
  return rows.map((r) => r.path);
}

function rowsLeft(sqlite: Database): number {
  // bun:sqlite .get() is untyped; this query literally selects `c`.
  const row = sqlite.query("SELECT count(*) AS c FROM audit_log").get() as { c: number };
  return row.c;
}

describe("audit retention sweep", () => {
  test("deletes swept rows older than the window and keeps rows inside it", async () => {
    const sqlite = await fixture();
    seed(sqlite, [
      { ts: NOW - 91 * DAY_MS, path: `${SVC}/SessionsInput` },
      { ts: NOW - 200 * DAY_MS, path: `${SVC}/SessionsInput` },
      { ts: NOW - 89 * DAY_MS, path: `${SVC}/SessionsInput` },
      // Ancient, but not in the sweep allowlist: must survive on identity,
      // not on age.
      { ts: NOW - 200 * DAY_MS, path: `${SVC}/PairApprove` },
    ]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW });

    expect(deleted).toBe(2);
    // pathsLeft orders by id, i.e. seed order: the in-window SessionsInput was
    // seeded before PairApprove.
    expect(pathsLeft(sqlite)).toEqual([
      `${SVC}/SessionsInput`,
      `${SVC}/PairApprove`,
    ]);
  });

  // The test that stops someone "simplifying" this back into a blanket DELETE.
  test("never sweeps auth, pairing or lifecycle rows regardless of age", async () => {
    const sqlite = await fixture();
    const ancient = NOW - 5 * 365 * DAY_MS;
    seed(sqlite, [
      { ts: ancient, path: `${SVC}/SessionsInput` },
      { ts: ancient, path: `${SVC}/PairApprove` },
      { ts: ancient, path: `${SVC}/AuthRedeemBrowser` },
      { ts: ancient, path: `${SVC}/WorkersDelete` },
      { ts: ancient, path: `${SVC}/WorkspacesDelete` },
      { ts: ancient, path: `${SVC}/SessionsSpawn` },
    ]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW });

    expect(deleted).toBe(1);
    expect(pathsLeft(sqlite)).toEqual([
      `${SVC}/PairApprove`,
      `${SVC}/AuthRedeemBrowser`,
      `${SVC}/WorkersDelete`,
      `${SVC}/WorkspacesDelete`,
      `${SVC}/SessionsSpawn`,
    ]);
  });

  test("sweeps a method under any service prefix", async () => {
    const sqlite = await fixture();
    seed(sqlite, [
      { ts: NOW - 91 * DAY_MS, path: "/eddy.v1.CoordinatorService/SessionsInput" },
      // Suffix match must be anchored on the path separator, not a substring.
      { ts: NOW - 91 * DAY_MS, path: `${SVC}/SessionsInputReplay` },
    ]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW });

    expect(deleted).toBe(1);
    expect(pathsLeft(sqlite)).toEqual([`${SVC}/SessionsInputReplay`]);
  });

  test("loops over multiple batches", async () => {
    const sqlite = await fixture();
    // 2500 sweepable rows at 1000 per batch. A single unbounded-LIMIT pass
    // would report 1000 and leave 1500 behind.
    const stale = Array.from({ length: 2_500 }, (_, i) => ({
      ts: NOW - (91 * DAY_MS + i),
      path: `${SVC}/SessionsInput`,
    }));
    seed(sqlite, [...stale, { ts: NOW - DAY_MS, path: `${SVC}/SessionsInput` }]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW, batchSize: 1_000 });

    expect(deleted).toBe(2_500);
    expect(rowsLeft(sqlite)).toBe(1);
  });

  test("an exact multiple of the batch size terminates and deletes everything", async () => {
    const sqlite = await fixture();
    seed(sqlite, Array.from({ length: 200 }, (_, i) => ({
      ts: NOW - (91 * DAY_MS + i),
      path: `${SVC}/SessionsInput`,
    })));

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW, batchSize: 100 });

    expect(deleted).toBe(200);
    expect(rowsLeft(sqlite)).toBe(0);
  });

  test("an empty table is a no-op", async () => {
    const sqlite = await fixture();

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW });

    expect(deleted).toBe(0);
    expect(rowsLeft(sqlite)).toBe(0);
  });

  test("a table with nothing old enough is a no-op", async () => {
    const sqlite = await fixture();
    seed(sqlite, [
      { ts: NOW - 10 * DAY_MS, path: `${SVC}/SessionsInput` },
      { ts: NOW - 10 * DAY_MS, path: `${SVC}/PairApprove` },
    ]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 90, now: NOW });

    expect(deleted).toBe(0);
    expect(rowsLeft(sqlite)).toBe(2);
  });

  test("honours a non-default window", async () => {
    const sqlite = await fixture();
    seed(sqlite, [
      { ts: NOW - 40 * DAY_MS, path: `${SVC}/SessionsInput` },
      { ts: NOW - 20 * DAY_MS, path: `${SVC}/SessionsInput` },
    ]);

    const deleted = await sweepAuditLog(sqlite, { retentionDays: 30, now: NOW });

    expect(deleted).toBe(1);
    expect(rowsLeft(sqlite)).toBe(1);
  });
  test("one-time cleanup removes only anonymous successful static reads", async () => {
    const sqlite = await fixture();
    const insert = sqlite.query(
      `INSERT INTO audit_log
       (ts, caller_fp, method, path, status)
       VALUES (?, ?, ?, ?, ?)`,
    );
    const rows: Array<[string | null, string, string, number]> = [
      [null, "GET", "/", 200],
      [null, "HEAD", "/assets/app.js", 304],
      [null, "GET", "/login", 404],
      [null, "POST", "/", 200],
      ["authenticated-fp", "GET", "/", 200],
      [null, "GET", "/api/db-export", 200],
      [null, "GET", "/api/security-event", 200],
      [null, "GET", "/internal/lifecycle", 200],
      [null, "GET", `${SVC}/MiscHealth`, 200],
    ];
    for (const [callerFp, method, path, status] of rows) {
      insert.run(NOW, callerFp, method, path, status);
    }

    const deleted = await cleanupAnonymousStaticAuditLog(sqlite, 1);

    expect(deleted).toBe(2);
    expect(pathsLeft(sqlite)).toEqual([
      "/login",
      "/",
      "/",
      "/api/db-export",
      "/api/security-event",
      "/internal/lifecycle",
      `${SVC}/MiscHealth`,
    ]);
  });

});
