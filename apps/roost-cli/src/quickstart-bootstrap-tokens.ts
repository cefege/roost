// Mint one-shot coordinator bootstrap tokens by writing directly into the
// coordinator SQLite database. Quickstart runs on the coord host and owns
// the DB file, so tokens are created before any service exists — no RPC and
// no loopback bypass involved. The row shape mirrors coord's authMintBootstrap:
// the public redeem endpoints (authRedeemBrowser / authRedeemWorker) reject
// anything else, so this schema is load-bearing across apps.

import { Database } from "bun:sqlite";

const BOOTSTRAP_TOKEN_TTL_MS = 24 * 60 * 60 * 1000;

function insertBootstrapToken(
  databasePath: string,
  kind: "browser" | "worker",
  label: string,
): string {
  const rand = new Uint8Array(18);
  crypto.getRandomValues(rand);
  const token = "roost_bt_" + Array.from(rand).map((b) => b.toString(16).padStart(2, "0")).join("");
  const now = Date.now();
  const db = new Database(databasePath);
  try {
    db.query(
      `INSERT INTO bootstrap_tokens (token, kind, label, created_at_ms, expires_at_ms, used_at_ms, used_by_fp)
       VALUES (?, ?, ?, ?, ?, NULL, NULL)`,
    ).run(token, kind, label, now, now + BOOTSTRAP_TOKEN_TTL_MS);
  } finally {
    db.close();
  }
  return token;
}

/** Browser bootstrap token: the host browser redeems it via the public
 *  authRedeemBrowser on first load (#pair fragment). */
export function mintBrowserToken(databasePath: string, label: string): string {
  return insertBootstrapToken(databasePath, "browser", label);
}

/** Worker bootstrap token (kind "worker"): authRedeemWorker enrolls the
 *  worker's key from it — binary-mode quickstart uses this so the local
 *  worker authorizes on a fresh install without a manual token. */
export function mintWorkerToken(databasePath: string, label: string): string {
  return insertBootstrapToken(databasePath, "worker", label);
}
