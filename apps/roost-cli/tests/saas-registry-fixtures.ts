// Split SaaS registry suites need identical deterministic IDs and isolated temporary databases.
// This module keeps that setup in one place while giving each test file its own cleanup queue.
// The shared constants preserve the original assertions without coupling the groups at runtime.

import { Database } from "bun:sqlite";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { SaasRegistry } from "../src/saas/registry.ts";

export const ACCOUNT_ID = "11111111-1111-4111-8111-111111111111";
export const COORDINATOR_ID = "22222222-2222-4222-8222-222222222222";
export const SECOND_COORDINATOR_ID = "33333333-3333-4333-8333-333333333333";
export const SECOND_ACCOUNT_ID = "44444444-4444-4444-8444-444444444444";
export const JOB_ID = "55555555-5555-4555-8555-555555555555";
export const SECOND_JOB_ID = "66666666-6666-4666-8666-666666666666";
export const TICKET_JTI = "77777777-7777-4777-8777-777777777777";
export const SECOND_TICKET_JTI = "88888888-8888-4888-8888-888888888888";
export const WRONG_LEASE_TOKEN = "99999999-9999-4999-8999-999999999999";
export const GOOGLE_SUBJECT = "google-subject-owner";
export const DEVICE_FP = "d".repeat(64);
export const IMAGE = `sha256:${"a".repeat(64)}`;
export const ROUTE_A = "a".repeat(64);
export const ROUTE_B = "b".repeat(64);

export function createSaasRegistryFixtureScope() {
  const cleanups: string[] = [];

  function cleanup(): void {
    while (cleanups.length > 0) rmSync(cleanups.pop()!, { recursive: true, force: true });
  }

  function fixture(nowRef = { value: 1_000 }): {
    dir: string;
    root: string;
    path: string;
    registry: SaasRegistry;
    nowRef: { value: number };
  } {
    const dir = mkdtempSync(join(tmpdir(), "roost-saas-registry-"));
    cleanups.push(dir);
    const root = join(dir, "control-root");
    const path = join(root, "control.db");
    const ids = [ACCOUNT_ID, COORDINATOR_ID, JOB_ID, SECOND_JOB_ID];
    const registry = new SaasRegistry({
      rootDir: root,
      path,
      now: () => nowRef.value,
      createId: () => {
        const id = ids.shift();
        if (!id) throw new Error("test UUID source exhausted");
        return id;
      },
    });
    return { dir, root, path, registry, nowRef };
  }

  function rawAccountsFixture(
    rows: Array<{ id: string; email: string; routeKey?: string | null }>,
    includeRouteKey: boolean,
  ): { root: string; path: string } {
    const dir = mkdtempSync(join(tmpdir(), "roost-saas-registry-raw-"));
    cleanups.push(dir);
    const root = join(dir, "control-root");
    const path = join(root, "control.db");
    mkdirSync(root, { recursive: true });
    const sqlite = new Database(path, { create: true });
    try {
      sqlite.exec(`
        CREATE TABLE accounts (
          id TEXT PRIMARY KEY,
          email_normalized TEXT NOT NULL UNIQUE,
          ${includeRouteKey ? "route_key TEXT," : ""}
          state TEXT NOT NULL,
          created_at_ms INTEGER NOT NULL,
          activated_at_ms INTEGER,
          disabled_at_ms INTEGER
        );
        PRAGMA user_version=1;
      `);
      for (const row of rows) {
        if (includeRouteKey) {
          sqlite.query(`
            INSERT INTO accounts (
              id, email_normalized, route_key, state, created_at_ms, activated_at_ms, disabled_at_ms
            ) VALUES (?, ?, ?, 'pending', 1000, NULL, NULL)
          `).run(row.id, row.email, row.routeKey ?? null);
        } else {
          sqlite.query(`
            INSERT INTO accounts (
              id, email_normalized, state, created_at_ms, activated_at_ms, disabled_at_ms
            ) VALUES (?, ?, 'pending', 1000, NULL, NULL)
          `).run(row.id, row.email);
        }
      }
    } finally {
      sqlite.close();
    }
    return { root, path };
  }

  return { cleanup, fixture, rawAccountsFixture };
}
