import { randomUUID } from "node:crypto";
import { lstat, mkdir, readFile, realpath, rename, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { sha256, sourceCommand } from "./source-command";
import { installNativeBinding, type NativeIdentity } from "./source-native";

export interface PreparedSource {
  root: string;
  revision: string;
  patchHashes: string[];
}

interface SourcePin {
  repository: string;
  revision: string;
  patches: string[];
}

interface SourceIdentity {
  schema: 1;
  repository: string;
  revision: string;
  patches: { path: string; sha256: string }[];
  tree: string;
  bunVersion: string;
  native: NativeIdentity;
}

export const sourceMarker = ".git/roost-parity-source.json";

export async function sourceEnvironment(repoRoot: string): Promise<Record<string, string>> {
  const home = join(repoRoot, "target/omp-parity/source-home");
  const temporary = join(home, "tmp");
  await mkdir(temporary, { recursive: true });
  return {
    PATH: process.env.PATH ?? dirname(process.execPath),
    HOME: home,
    TMPDIR: temporary,
    TMP: temporary,
    TEMP: temporary,
    XDG_CACHE_HOME: join(home, "cache"),
    XDG_CONFIG_HOME: join(home, "config"),
    XDG_DATA_HOME: join(home, "data"),
    BUN_INSTALL_CACHE_DIR: join(home, "bun-cache"),
    PI_NATIVES_DIR: join(home, "natives"),
    CI: "1",
    TERM: "xterm-256color",
  };
}

async function readPin(repoRoot: string): Promise<SourcePin> {
  const pin = JSON.parse(await readFile(join(repoRoot, "deploy/omp/source.json"), "utf8")) as SourcePin;
  if (!/^https:\/\/[^\s]+\.git$/.test(pin.repository) || !/^[0-9a-f]{40}$/.test(pin.revision)
    || !Array.isArray(pin.patches) || pin.patches.length === 0
    || pin.patches.some(path => !/^[a-zA-Z0-9][a-zA-Z0-9._-]*\.patch$/.test(path))) {
    throw new Error("deploy/omp/source.json must name an HTTPS repository, exact revision and ordered local patches.");
  }
  return pin;
}

async function pinnedPatches(repoRoot: string, pin: SourcePin): Promise<SourceIdentity["patches"]> {
  return Promise.all(pin.patches.map(async path => ({
    path,
    sha256: sha256(await readFile(join(repoRoot, "deploy/omp", path))),
  })));
}

async function sourceTree(
  repoRoot: string,
  root: string,
  env: Record<string, string>,
  pin?: SourcePin,
): Promise<string> {
  const gitDirectory = await lstat(join(root, ".git"));
  if (!gitDirectory.isDirectory() || gitDirectory.isSymbolicLink()) {
    throw new Error(`Refusing OMP checkout with an external or non-directory .git: ${root}`);
  }
  const index = join(repoRoot, "target/omp-parity", `source-index-${randomUUID()}`);
  await mkdir(dirname(index), { recursive: true });
  const indexedEnv = { ...env, GIT_INDEX_FILE: index };
  try {
    await sourceCommand(["git", "read-tree", pin?.revision ?? "HEAD"], root, indexedEnv);
    if (pin) {
      for (const patch of pin.patches) {
        await sourceCommand(["git", "apply", "--cached", join(repoRoot, "deploy/omp", patch)], root, indexedEnv);
      }
    } else {
      await sourceCommand(["git", "add", "--all", "--", "."], root, indexedEnv);
    }
    return await sourceCommand(["git", "write-tree"], root, indexedEnv);
  } finally {
    await rm(index, { force: true });
    await rm(`${index}.lock`, { force: true });
  }
}

async function assertPinnedTree(repoRoot: string, root: string, pin: SourcePin, env: Record<string, string>): Promise<string> {
  const expected = await sourceTree(repoRoot, root, env, pin);
  const actual = await sourceTree(repoRoot, root, env);
  if (actual !== expected) {
    throw new Error(`Refusing changed OMP source at ${root}: tree ${actual} is not pinned base + ordered patches (${expected}). Preserve it and choose a fresh owned checkout; nothing was reset.`);
  }
  return actual;
}

async function finishPreparation(repoRoot: string, root: string, pin: SourcePin, existingNative?: NativeIdentity): Promise<PreparedSource> {
  const env = await sourceEnvironment(repoRoot);
  const tree = await assertPinnedTree(repoRoot, root, pin, env);
  await sourceCommand([process.execPath, "install", "--frozen-lockfile", "--ignore-scripts"], root, env);
  const native = await installNativeBinding(repoRoot, root, env, existingNative);
  await assertPinnedTree(repoRoot, root, pin, env);
  const identity: SourceIdentity = {
    schema: 1,
    repository: pin.repository,
    revision: pin.revision,
    patches: await pinnedPatches(repoRoot, pin),
    tree,
    bunVersion: Bun.version,
    native,
  };
  const marker = join(root, sourceMarker);
  const temporaryMarker = `${marker}.${randomUUID()}`;
  await writeFile(temporaryMarker, `${JSON.stringify(identity, null, 2)}\n`, { flag: "wx" });
  await rename(temporaryMarker, marker);
  await writeFile(join(repoRoot, "target/omp-parity/source-identity.json"), `${JSON.stringify(identity, null, 2)}\n`);
  return { root, revision: pin.revision, patchHashes: identity.patches.map(patch => patch.sha256) };
}

/** Explicit adoption for an operator-owned checkout; its entire source tree must match the pin. */
export async function markPreparedSource(repoRoot: string): Promise<PreparedSource> {
  repoRoot = await realpath(repoRoot);
  const root = join(repoRoot, "target/omp-parity-source");
  const stat = await lstat(root);
  if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`Refusing non-directory OMP checkout: ${root}`);
  return finishPreparation(repoRoot, root, await readPin(repoRoot));
}

export async function prepareSource(repoRoot: string): Promise<PreparedSource> {
  repoRoot = await realpath(repoRoot);
  const pin = await readPin(repoRoot);
  const root = join(repoRoot, "target/omp-parity-source");
  const env = await sourceEnvironment(repoRoot);
  let exists = true;
  let existingNative: NativeIdentity | undefined;
  try {
    const stat = await lstat(root);
    if (!stat.isDirectory() || stat.isSymbolicLink()) throw new Error(`Refusing non-directory OMP checkout: ${root}`);
  } catch (error) {
    if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
    exists = false;
  }
  if (exists) {
    let marked: SourceIdentity;
    try {
      marked = JSON.parse(await readFile(join(root, sourceMarker), "utf8")) as SourceIdentity;
    } catch {
      throw new Error(`Refusing unmarked OMP checkout at ${root}. Its contents were preserved; only an explicitly owned, matching checkout may be marked.`);
    }
    const patches = await pinnedPatches(repoRoot, pin);
    if (!marked || marked.schema !== 1 || marked.repository !== pin.repository || marked.revision !== pin.revision
      || !marked.native || JSON.stringify(marked.patches) !== JSON.stringify(patches)) {
      throw new Error(`Refusing stale OMP checkout at ${root}: source revision or ordered patch hashes changed. Nothing was deleted or reset.`);
    }
    const tree = await assertPinnedTree(repoRoot, root, pin, env);
    if (tree !== marked.tree) throw new Error(`Refusing invalid OMP ownership marker at ${root}.`);
    existingNative = marked.native;
  } else {
    await mkdir(dirname(root), { recursive: true });
    await mkdir(root);
    await sourceCommand(["git", "init", "--quiet"], root, env);
    await sourceCommand(["git", "remote", "add", "origin", pin.repository], root, env);
    await sourceCommand(["git", "fetch", "--depth", "1", "origin", pin.revision], root, env);
    await sourceCommand(["git", "checkout", "--detach", "FETCH_HEAD"], root, env);
    for (const patch of pin.patches) {
      await sourceCommand(["git", "apply", "--index", join(repoRoot, "deploy/omp", patch)], root, env);
    }
  }
  return finishPreparation(repoRoot, root, pin, existingNative);
}

if (import.meta.main) {
  const args = process.argv.slice(2);
  const mark = args.includes("--mark-owned");
  const rootFlag = args.indexOf("--repo-root");
  const repoRoot = rootFlag === -1 ? resolve(import.meta.dir, "../../..") : args[rootFlag + 1];
  const expected = mark ? 1 : 0;
  if (!repoRoot || args.length !== expected + (rootFlag === -1 ? 0 : 2)
    || args.some((arg, index) => arg !== "--mark-owned" && arg !== "--repo-root" && index !== rootFlag + 1)) {
    throw new Error("Usage: bun run deploy/omp/parity/source.ts [--repo-root PATH] [--mark-owned]");
  }
  const prepared = await (mark ? markPreparedSource(repoRoot) : prepareSource(repoRoot));
  console.log(JSON.stringify(prepared));
}
