/**
 * Persists private provisioner requests and their canonical responses for replay handling.
 * The IPC server reserves each signed request before invoking privileged provisioning work.
 * Durable byte equality makes retries idempotent while rejecting nonce reuse with changed input.
 */

import { Database } from "bun:sqlite";
import { chmodSync, mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { Buffer } from "node:buffer";

export type ReplayReservation = { state: "new" } | { state: "pending" } | { state: "replay"; response: Buffer } | { state: "mismatch" };
interface ReplayRow { request_bytes: Uint8Array; response_bytes: Uint8Array | null }

export class ProvisionerReplayStore {
  readonly #database: Database;
  readonly #maxRows: number;
  readonly #retentionMs: number;

  constructor(options: { path: string; maxRows?: number; retentionMs?: number }) {
    this.#maxRows = options.maxRows ?? 10_000;
    this.#retentionMs = options.retentionMs ?? 10 * 60 * 1_000;
    if (!Number.isSafeInteger(this.#maxRows) || this.#maxRows <= 0 || !Number.isSafeInteger(this.#retentionMs) || this.#retentionMs < 60_000) throw new Error("invalid provisioner replay-store bounds");
    mkdirSync(dirname(options.path), { recursive: true, mode: 0o700 });
    this.#database = new Database(options.path, { create: true, strict: true });
    chmodSync(options.path, 0o600);
    this.#database.run("PRAGMA journal_mode = WAL");
    this.#database.run("PRAGMA synchronous = FULL");
    this.#database.run(`CREATE TABLE IF NOT EXISTS provisioner_ipc_replays (
      nonce TEXT PRIMARY KEY NOT NULL,
      request_bytes BLOB NOT NULL,
      response_bytes BLOB,
      created_at_ms INTEGER NOT NULL,
      completed_at_ms INTEGER,
      CHECK (length(nonce) = 43),
      CHECK (length(request_bytes) BETWEEN 1 AND 16384),
      CHECK (response_bytes IS NULL OR length(response_bytes) BETWEEN 1 AND 16384)
    ) STRICT`);
  }

  reserve(nonce: string, request: Uint8Array, nowMs: number): ReplayReservation {
    return this.#database.transaction(() => {
      this.#database.query("DELETE FROM provisioner_ipc_replays WHERE created_at_ms < ?").run(nowMs - this.#retentionMs);
      const row = this.#database.query("SELECT request_bytes, response_bytes FROM provisioner_ipc_replays WHERE nonce = ?").get(nonce) as ReplayRow | null;
      if (row) {
        if (!Buffer.from(row.request_bytes).equals(Buffer.from(request))) return { state: "mismatch" } as const;
        return row.response_bytes === null ? { state: "pending" } as const : { state: "replay", response: Buffer.from(row.response_bytes) } as const;
      }
      const { count } = this.#database.query("SELECT count(*) AS count FROM provisioner_ipc_replays").get() as { count: number };
      if (count >= this.#maxRows) throw new Error("provisioner IPC replay table is full");
      this.#database.query("INSERT INTO provisioner_ipc_replays (nonce, request_bytes, created_at_ms) VALUES (?, ?, ?)").run(nonce, request, nowMs);
      return { state: "new" } as const;
    })();
  }

  complete(nonce: string, request: Uint8Array, response: Uint8Array, nowMs: number): void {
    if (response.byteLength === 0 || response.byteLength > 16_384) throw new Error("provisioner IPC response is outside bounds");
    const result = this.#database.query("UPDATE provisioner_ipc_replays SET response_bytes = ?, completed_at_ms = ? WHERE nonce = ? AND request_bytes = ? AND response_bytes IS NULL").run(response, nowMs, nonce, request);
    if (result.changes !== 1) throw new Error("provisioner IPC replay completion lost ownership");
  }

  close(): void { this.#database.close(); }
}
