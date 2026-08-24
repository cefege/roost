// The bun -e program transmitted by _remoteDeployLockCommands in
// deploy-exec.ts to acquire/renew/release the SQLite-backed remote deploy
// lease on darwin/linux targets. Kept as its own module so deploy-exec
// stays under the size ratchet; the string itself must remain byte-stable
// because journal-recovery tooling compares transmitted commands.

export const REMOTE_DEPLOY_LOCK_PROGRAM = String.raw`
import { Database } from "bun:sqlite";
import { chmodSync } from "node:fs";

class LockExit extends Error {
  constructor(code, message) {
    super(message);
    this.code = code;
  }
}

const action = process.env.ROOST_DEPLOY_LOCK_ACTION ?? "";
const lockPath = process.env.ROOST_DEPLOY_LOCK_PATH ?? "";
const owner = process.env.ROOST_DEPLOY_LOCK_OWNER ?? "";
const leaseSeconds = Number(process.env.ROOST_DEPLOY_LOCK_LEASE);
if (!["acquire", "renew", "release"].includes(action)
  || !lockPath
  || !owner
  || !Number.isSafeInteger(leaseSeconds)
  || leaseSeconds < 1) {
  console.error("invalid remote deployment lock request");
  process.exit(64);
}

const database = new Database(lockPath, { create: true });
let transactionOpen = false;
try {
  chmodSync(lockPath, 0o600);
  database.exec("PRAGMA busy_timeout = 30000");
  database.exec("PRAGMA journal_mode = DELETE");
  database.exec("PRAGMA synchronous = FULL");
  database.exec(
    "CREATE TABLE IF NOT EXISTS deploy_lease (" +
    "singleton INTEGER PRIMARY KEY CHECK (singleton = 1), " +
    "owner TEXT NOT NULL, created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL)",
  );
  database.exec("BEGIN IMMEDIATE");
  transactionOpen = true;
  if (action === "acquire") {
    const activeTransactionTable = database.query(
      "SELECT name FROM sqlite_schema " +
      "WHERE type = 'table' AND name = 'active_machine_transaction'",
    ).get();
    if (activeTransactionTable) {
      const activeTransaction = database.query(
        "SELECT 1 AS occupied FROM active_machine_transaction WHERE singleton = 1",
      ).get();
      if (activeTransaction) {
        throw new LockExit(75, "another machine transaction is active at " + lockPath);
      }
    }
  }


  const now = Math.floor(Date.now() / 1000);
  const current = database.query(
    "SELECT owner, created_at, expires_at FROM deploy_lease WHERE singleton = 1",
  ).get();
  if (current) {
    const valid = typeof current.owner === "string"
      && current.owner.length > 0
      && Number.isSafeInteger(current.created_at)
      && current.created_at >= 0
      && Number.isSafeInteger(current.expires_at)
      && current.expires_at >= 0;
    if (!valid) throw new LockExit(75, "deployment lock record is malformed");
  }

  if (action === "acquire") {
    if (current && current.owner !== owner && current.expires_at > now) {
      throw new LockExit(75, "another deployment owns " + lockPath + " and its lease is still active");
    }
    database.query(
      "INSERT INTO deploy_lease (singleton, owner, created_at, expires_at) VALUES (1, ?, ?, ?) " +
      "ON CONFLICT(singleton) DO UPDATE SET owner = excluded.owner, " +
      "created_at = excluded.created_at, expires_at = excluded.expires_at",
    ).run(owner, now, now + leaseSeconds);
  } else if (action === "renew") {
    if (!current || current.owner !== owner || current.expires_at <= now) {
      throw new LockExit(74, "deployment lock ownership was lost or its lease expired");
    }
    database.query(
      "UPDATE deploy_lease SET expires_at = ? WHERE singleton = 1 AND owner = ?",
    ).run(now + leaseSeconds, owner);
  } else {
    database.query(
      "DELETE FROM deploy_lease WHERE singleton = 1 AND owner = ?",
    ).run(owner);
  }

  database.exec("COMMIT");
  transactionOpen = false;
} catch (error) {
  if (transactionOpen) {
    try {
      database.exec("ROLLBACK");
    } catch {}
  }
  const code = error instanceof LockExit ? error.code : 75;
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = code;
} finally {
  try {
    database.close();
  } catch {}
}
`;
