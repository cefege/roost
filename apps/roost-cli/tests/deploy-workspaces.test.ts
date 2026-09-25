// A staged macOS release installs with `bun install --frozen-lockfile` against
// the canonical root package.json + bun.lock, so every workspace the lockfile
// declares must exist in the staged tree. The set used to be hand-written and
// went stale the moment a workspace joined the root `workspaces` globs: every
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
    const root = fixture(["apps/*", "packages/*", "smoke"], [
      "apps/worker", "apps/coord", "apps/web", "apps/roost-cli", "smoke",
      "packages/host", "packages/observability", "packages/platform", "packages/protocol", "packages/wterm",
    ]);
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/roost-cli", "smoke"]);
  });

  test("a workspace added under an existing glob is picked up with no code change", () => {
    const root = fixture(["apps/*"], ["apps/worker", "apps/brand-new"]);
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/brand-new"]);
  });

  test("a glob member without a manifest is not a workspace and is never staged", () => {
    const root = fixture(["apps/*"], ["apps/worker", "apps/roost-cli"]);
    mkdirSync(join(root, "apps", "scratch"), { recursive: true });
    expect(manifestOnlyWorkspaces(root)).toEqual(["apps/roost-cli"]);
  });

  test("this repo's real manifest resolves every non-rsynced workspace", () => {
    // The regression itself: every manifest the real root declares and that
    // is not rsynced whole has to reach the staged set. `smoke` is the one
    // that regressed silently before — it is not an `apps/` or `packages/`
    // member, so a hand-written list kept missing it.
    expect(manifestOnlyWorkspaces(join(import.meta.dir, "..", "..", "..")))
      .toContain("smoke");
  });
});
