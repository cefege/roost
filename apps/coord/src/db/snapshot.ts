// Integrity-checked VACUUM INTO snapshot of the live coordinator DB, used as
// the payload for coordinator moves. Must use query() not prepare(): a
// prepared statement is finalized only on GC and keeps the source handle
// busy, so coord's own close() then fails with "database is locked". A failed
// run removes destPath — partial snapshots must never escape to the target.
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import { Database } from "bun:sqlite";

const HASH_CHUNK_SIZE = 1024 * 1024;

/** Create a standalone, integrity-checked snapshot of a live SQLite database. */
export function createSqliteSnapshot(sqlite: Database, destPath: string): { size: number; sha256: string } {
  try {
    fs.rmSync(destPath, { force: true });
    // query(), not prepare(): a prepare()d Statement is finalized only on GC, so
    // it keeps the source handle busy — the coordinator's own close(true) then
    // reports "database is locked" after any snapshot. query() is cache-owned.
    sqlite.query("VACUUM INTO ?").run(destPath);
    fs.chmodSync(destPath, 0o600);

    const snapshot = new Database(destPath, { readonly: true });
    try {
      const result = snapshot.query<{ integrity_check: string }, []>("PRAGMA integrity_check").get();
      if (result?.integrity_check !== "ok") {
        throw new Error(`SQLite snapshot integrity_check failed: ${result?.integrity_check ?? "no result"}`);
      }
    } finally {
      snapshot.close();
    }

    const size = fs.statSync(destPath).size;
    const hasher = createHash("sha256");
    const chunk = new Uint8Array(HASH_CHUNK_SIZE);
    const fd = fs.openSync(destPath, "r");
    try {
      for (;;) {
        const bytesRead = fs.readSync(fd, chunk, 0, chunk.length, null);
        if (bytesRead === 0) break;
        hasher.update(chunk.subarray(0, bytesRead));
      }
    } finally {
      fs.closeSync(fd);
    }
    return { size, sha256: hasher.digest("hex") };
  } catch (error) {
    fs.rmSync(destPath, { force: true });
    throw error;
  }
}
