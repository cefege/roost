// A staged macOS release installs with `bun install --frozen-lockfile` against
// the canonical root package.json + bun.lock, so every workspace the lockfile
// declares must exist in the staged tree. The set used to be hand-written and
// went stale the moment apps/site joined the root `workspaces` globs: every
// macOS deploy then failed with "lockfile had changes, but lockfile is frozen".

import { describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { manifestOnlyWorkspaces } from "../src/deploy-workspaces.ts";

function fixture(workspaces: readonly string[], dirs: readonly string[]): string {
  const root = mkdtempSync(join(tmpdir(), "roost-workspaces-"));
  writeFileSync(join(root, "package.json"), JSON.stringify({ name: "roost", workspaces }));
  for (const relative of dirs) {
    mkdirSync(join(root, relative), { recursive: true });
    writeFileSync(join(root, relative, "package.json"), JSON.stringify({ name: relative }));
  }
  return root;
}

describe("manifestOnlyWorkspaces", () => {
  test("expands globs and drops the workspaces rsynced whole", () => {
    const root = fixture(["apps/*", "smoke"], [
      "apps/worker", "apps/shared", "apps/coord", "apps/web",
      "apps/roost-cli", "apps/site", "smoke",
    ]);
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/roost-cli", "apps/site", "smoke"]);
  });

  test("a workspace added under an existing glob is picked up with no code change", () => {
    const root = fixture(["apps/*"], ["apps/worker", "apps/brand-new"]);
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/brand-new"]);
  });

  test("a glob member without a manifest is not a workspace and is never staged", () => {
    const root = fixture(["apps/*"], ["apps/worker", "apps/site"]);
    mkdirSync(join(root, "apps", "scratch"), { recursive: true });
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/site"]);
  });

  test("this repo's real manifest resolves every non-rsynced workspace", () => {
    // The regression itself: apps/site must be in the staged set.
    expect(manifestOnlyWorkspaces(join(import.meta.dir, "..", "..", "..")))
      .toContain("apps/site");
  });
});
