// Pinned source identity tests use disposable Git checkouts.
// They pin exact HEAD, canonical root, and clean tracked/untracked requirements.
// Failure metadata remains source_unavailable without parsing Git diagnostics.

import { afterEach, describe, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DeployFailure, resolvePinnedSourceShaOrDie } from "../src/deploy-exec.ts";

interface PinnedCheckout {
  root: string;
  sha: string;
}

const roots: string[] = [];

function gitOutput(root: string, args: string[]): string {
  const result = Bun.spawnSync(["git", ...args], { cwd: root });
  expect(result.exitCode, result.stderr.toString()).toBe(0);
  return result.stdout.toString().trim();
}

function createPinnedCheckout(): PinnedCheckout {
  const root = mkdtempSync(join(tmpdir(), "roost-pinned-source-"));
  roots.push(root);
  gitOutput(root, ["init", "--quiet"]);
  gitOutput(root, ["config", "user.email", "test@example.test"]);
  gitOutput(root, ["config", "user.name", "Roost Test"]);
  mkdirSync(join(root, "apps", "worker", "src"), { recursive: true });
  writeFileSync(join(root, "apps", "worker", "src", "main.ts"), "export {};\n");
  gitOutput(root, ["add", "."]);
  gitOutput(root, ["commit", "--quiet", "-m", "pinned source"]);
  return { root, sha: gitOutput(root, ["rev-parse", "HEAD"]) };
}

function expectSourceUnavailable(action: () => void): void {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(DeployFailure);
    expect(error).toMatchObject({
      workerUpdateFailure: {
        code: "source_unavailable",
        phase: "preflight",
      },
    });
    return;
  }
  throw new Error("expected pinned source refusal");
}

afterEach(() => {
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("resolvePinnedSourceShaOrDie", () => {
  test("accepts a clean canonical checkout at its exact requested HEAD", () => {
    const checkout = createPinnedCheckout();
    expect(resolvePinnedSourceShaOrDie(checkout.root, checkout.sha)).toBe(checkout.sha);
  });

  test("refuses dirty, mismatched, and noncanonical source roots", () => {
    const checkout = createPinnedCheckout();
    writeFileSync(join(checkout.root, "untracked.txt"), "dirty\n");
    expectSourceUnavailable(() => resolvePinnedSourceShaOrDie(checkout.root, checkout.sha));
    rmSync(join(checkout.root, "untracked.txt"));
    expectSourceUnavailable(() => resolvePinnedSourceShaOrDie(checkout.root, "a".repeat(40)));
    expectSourceUnavailable(() => resolvePinnedSourceShaOrDie(`${checkout.root}/.`, checkout.sha));
  });
});
