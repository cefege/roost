// Owns the SQLite connection and construction contract for the SaaS registry.
// Domain-specific registry stores inherit its protected transaction state.
// Initialization keeps directory permissions and schema setup atomic to open.
import { Database } from "bun:sqlite";
import { randomUUID } from "node:crypto";
import { chmodSync, mkdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import type { OpenSaasRegistryOptions } from "./registry-model.ts";
import { initialize } from "./registry-schema.ts";
import { DEFAULT_ROOT, createTenantRouteKey } from "./registry-validation.ts";

export class RegistryStorage {
  readonly path: string;
  readonly rootDir: string;
  protected readonly sqlite: Database;
  protected readonly now: () => number;
  protected readonly createId: () => string;
  protected readonly createRouteKey: () => string;

  constructor(options: OpenSaasRegistryOptions = {}) {
    this.rootDir = resolve(options.rootDir ?? DEFAULT_ROOT);
    this.path = resolve(options.path ?? join(this.rootDir, "control.db"));
    this.now = options.now ?? Date.now;
    this.createId = options.createId ?? randomUUID;
    this.createRouteKey = options.createRouteKey ?? createTenantRouteKey;
    const parent = dirname(this.path);
    mkdirSync(parent, { recursive: true, mode: 0o700 });
    chmodSync(parent, 0o700);
    const sqlite = new Database(this.path, { create: true });
    this.sqlite = sqlite;
    try {
      chmodSync(this.path, 0o600);
      initialize(sqlite, this.createRouteKey);
    } catch (error) {
      sqlite.close();
      throw error;
    }
  }

  close(): void {
    if (this.sqlite.inTransaction) this.sqlite.exec("ROLLBACK");
    this.sqlite.close();
  }
}
