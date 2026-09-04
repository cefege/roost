/**
 * Covers the managed-container SQLite page ceiling that bounds coordinator storage.
 * Bun discovers this suite directly and gives it an isolated temporary directory.
 * The cases depend on the production database opener and clean up every database.
 */
import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  MANAGED_SQLITE_MAX_BYTES,
  openDb,
} from "../src/db/connection.ts";

let workdir: string;

beforeAll(() => {
  workdir = mkdtempSync(join(tmpdir(), "roost-resource-caps-"));
});

afterAll(() => {
  if (workdir && existsSync(workdir)) {
    rmSync(workdir, { recursive: true, force: true });
  }
});

describe("managed SQLite page ceiling", () => {
  test("only managed-container opens set the fixed 1 GiB page cap", async () => {
    const generic = openDb(join(workdir, "generic.db"));
    const selfHosted = openDb(join(workdir, "self-hosted.db"), {
      managedContainer: false,
    });
    const managed = openDb(join(workdir, "managed.db"), {
      managedContainer: true,
    });
    try {
      const pageSize = Number(
        managed.sqlite.query<{ page_size: number }, []>("PRAGMA page_size").get()?.page_size,
      );
      const expectedPages = Math.floor(MANAGED_SQLITE_MAX_BYTES / pageSize);
      const genericPages = Number(
        generic.sqlite.query<{ max_page_count: number }, []>("PRAGMA max_page_count").get()?.max_page_count,
      );
      const selfHostedPages = Number(
        selfHosted.sqlite.query<{ max_page_count: number }, []>("PRAGMA max_page_count").get()?.max_page_count,
      );
      const managedPages = Number(
        managed.sqlite.query<{ max_page_count: number }, []>("PRAGMA max_page_count").get()?.max_page_count,
      );
      expect(managedPages).toBe(expectedPages);
      expect(managedPages * pageSize).toBe(MANAGED_SQLITE_MAX_BYTES);
      expect(genericPages).toBeGreaterThan(expectedPages);
      expect(selfHostedPages).toBeGreaterThan(expectedPages);
    } finally {
      await Promise.all([generic.close(), selfHosted.close(), managed.close()]);
    }
  });
});
