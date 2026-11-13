import { afterEach, expect, test } from "bun:test";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { sha256, sourceCommand } from "./source-command";
import { markPreparedSource, prepareSource, sourceEnvironment, sourceMarker } from "./source";

const repositories: string[] = [];
const revision = "650f3a108229ee11bf68ba3377f4e8c3391cd424";
const repository = "https://github.com/cefege/oh-my-pi.git";

async function scratchRepository(): Promise<string> {
  const root = await mkdtemp(join(tmpdir(), "roost-omp-source-safety-"));
  repositories.push(root);
  await mkdir(join(root, "deploy/omp"), { recursive: true });
  await mkdir(join(root, "target/omp-parity-source"), { recursive: true });
  await writeFile(join(root, "deploy/omp/source.json"), JSON.stringify({ repository, revision, patches: ["behavior.patch"] }));
  await writeFile(join(root, "deploy/omp/behavior.patch"), "an existing patch must not be rewritten\n");
  return root;
}

afterEach(async () => {
  await Promise.all(repositories.splice(0).map(root => rm(root, { recursive: true, force: true })));
});

test("an existing unmarked checkout is refused without changing its source", async () => {
  const root = await scratchRepository();
  const file = join(root, "target/omp-parity-source/operator-work.ts");
  await writeFile(file, "operator source edits\n");
  await expect(prepareSource(root)).rejects.toThrow("Refusing unmarked OMP checkout");
  expect(await readFile(file, "utf8")).toBe("operator source edits\n");
});

test("a stale owned checkout is refused without resetting source or marker", async () => {
  const root = await scratchRepository();
  const checkout = join(root, "target/omp-parity-source");
  const marker = join(checkout, sourceMarker);
  const receipt = JSON.stringify({ schema: 1, repository, revision: "0".repeat(40), patches: [], native: {} });
  await mkdir(dirname(marker), { recursive: true });
  await writeFile(marker, receipt);
  await writeFile(join(checkout, "operator-work.ts"), "retain me\n");
  await expect(prepareSource(root)).rejects.toThrow("Refusing stale OMP checkout");
  expect(await readFile(marker, "utf8")).toBe(receipt);
  expect(await readFile(join(checkout, "operator-work.ts"), "utf8")).toBe("retain me\n");
});

test("a symlinked checkout cannot redirect preparation into another directory", async () => {
  const root = await scratchRepository();
  const outside = join(root, "operator-checkout");
  const checkout = join(root, "target/omp-parity-source");
  await mkdir(outside);
  await writeFile(join(outside, "source.ts"), "external source\n");
  await rm(checkout, { recursive: true });
  await symlink(outside, checkout, "dir");
  await expect(prepareSource(root)).rejects.toThrow("Refusing non-directory OMP checkout");
  expect(await readFile(join(outside, "source.ts"), "utf8")).toBe("external source\n");
});

test("explicit adoption refuses a local source edit even with a matching revision and patch list", async () => {
  const root = await scratchRepository();
  const checkout = join(root, "target/omp-parity-source");
  const env = await sourceEnvironment(root);
  await sourceCommand(["git", "init", "--quiet"], checkout, env);
  await writeFile(join(checkout, "source.txt"), "base\n");
  await sourceCommand(["git", "add", "source.txt"], checkout, env);
  await sourceCommand(["git", "-c", "user.name=Parity Safety", "-c", "user.email=parity@example.invalid", "commit", "--quiet", "-m", "fixture source"], checkout, env);
  const base = await sourceCommand(["git", "rev-parse", "HEAD"], checkout, env);
  const patch = "diff --git a/source.txt b/source.txt\n--- a/source.txt\n+++ b/source.txt\n@@ -1 +1 @@\n-base\n+patched\n";
  await writeFile(join(root, "deploy/omp/source.json"), JSON.stringify({ repository, revision: base, patches: ["behavior.patch"] }));
  await writeFile(join(root, "deploy/omp/behavior.patch"), patch);
  await writeFile(join(checkout, "source.txt"), "local source edit\n");
  const marker = join(checkout, sourceMarker);
  const receipt = JSON.stringify({ schema: 1, repository, revision: base, patches: [{ path: "behavior.patch", sha256: sha256(patch) }], native: {} });
  await writeFile(marker, receipt);
  await expect(prepareSource(root)).rejects.toThrow("Refusing changed OMP source");
  await expect(markPreparedSource(root)).rejects.toThrow("Refusing changed OMP source");
  expect(await readFile(join(checkout, "source.txt"), "utf8")).toBe("local source edit\n");
  expect(await readFile(marker, "utf8")).toBe(receipt);
});

test("explicit adoption refuses an external Git directory before writing objects", async () => {
  const root = await scratchRepository();
  const checkout = join(root, "target/omp-parity-source");
  const external = join(root, "external-git");
  await mkdir(external);
  await writeFile(join(external, "operator-work"), "outside git state\n");
  await symlink(external, join(checkout, ".git"), "dir");
  await expect(markPreparedSource(root)).rejects.toThrow("external or non-directory .git");
  expect(await readFile(join(external, "operator-work"), "utf8")).toBe("outside git state\n");
});
