import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Credential, CredentialInfo, CredentialStore } from "@earendil-works/pi-ai";

export class SqliteCredentialStore implements CredentialStore {
  private readonly db: DatabaseSync;
  constructor(db: DatabaseSync) { this.db = db; }
  async read(providerId: string): Promise<Credential | undefined> {
    const row = this.db.prepare("SELECT json FROM credentials WHERE provider = ?").get(providerId) as { json: string } | undefined;
    return row ? JSON.parse(row.json) as Credential : undefined;
  }
  async list(): Promise<readonly CredentialInfo[]> {
    const rows = this.db.prepare("SELECT provider, json FROM credentials").all() as { provider: string; json: string }[];
    return rows.map(({ provider, json }) => ({ providerId: provider, type: (JSON.parse(json) as Credential).type }));
  }
  async modify(providerId: string, fn: (current: Credential | undefined) => Promise<Credential | undefined>): Promise<Credential | undefined> {
    this.db.exec("BEGIN IMMEDIATE");
    try {
      const currentRow = this.db.prepare("SELECT json FROM credentials WHERE provider = ?").get(providerId) as { json: string } | undefined;
      const updated = await fn(currentRow ? JSON.parse(currentRow.json) as Credential : undefined);
      if (updated) this.db.prepare("INSERT INTO credentials(provider,json) VALUES(?,?) ON CONFLICT(provider) DO UPDATE SET json=excluded.json").run(providerId, JSON.stringify(updated));
      this.db.exec("COMMIT");
      return updated;
    } catch (error) {
      this.db.exec("ROLLBACK");
      throw error;
    }
  }
  async delete(providerId: string): Promise<void> { this.db.prepare("DELETE FROM credentials WHERE provider = ?").run(providerId); }
}

export interface HostDatabase { db: DatabaseSync; credentials: SqliteCredentialStore; close(): void }

export function openHostDatabase(dataDir: string): HostDatabase {
  mkdirSync(dataDir, { recursive: true });
  const path = join(dataDir, "host.sqlite");
  mkdirSync(dirname(path), { recursive: true });
  const db = new DatabaseSync(path);
  db.exec("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;");
  db.exec(`CREATE TABLE IF NOT EXISTS conversations(id TEXT PRIMARY KEY, title TEXT NOT NULL, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL);
    CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
    CREATE TABLE IF NOT EXISTS credentials(provider TEXT PRIMARY KEY, json TEXT NOT NULL);`);
  return { db, credentials: new SqliteCredentialStore(db), close: () => db.close() };
}
