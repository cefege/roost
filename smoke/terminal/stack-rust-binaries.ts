// The Rust binaries the smoke knobs name must be CURRENT before a stack starts,
// or a mixed-stack run quietly proves last week's coordinator. This module owns
// that step: it decides whether the binary is stale against the workspace
// sources and invokes cargo when it is. Both launchers call it, so no run can
// reach a stale binary and no human has to remember to build first.

import { spawnSync } from "node:child_process";
import { readdirSync, statSync } from "node:fs";
import { join, relative, sep } from "node:path";

/** The workspace package and binary that produce every product executable. */
const CLI_PACKAGE = "roost-cli";
const CLI_BINARY = "roost";
/** Root files whose mtime decides whether a workspace build is current. */
const WORKSPACE_MANIFESTS = ["Cargo.toml", "Cargo.lock", "rust-toolchain.toml"] as const;

/**
 * The cargo profile a binary under `<checkout>/target/` belongs to, or `null`
 * when the path is not a build of this checkout.
 *
 * An installed copy under `/usr/local/bin` is the operator's to update, not a
 * harness's to overwrite, so it is reported and used exactly as given.
 */
export function cargoProfileFor(binaryPath: string, sourceRoot: string): string | null {
  const segments = relative(sourceRoot, binaryPath).split(sep);
  if (segments[0] !== "target") return null;
  const profile = segments[1];
  if (profile !== "debug" && profile !== "release") return null;
  if (segments.at(-1) !== CLI_BINARY) return null;
  return profile;
}

/** Newest mtime under `directory`, or 0 when the tree does not exist. */
function newestChangeMs(directory: string): number {
  let newest = 0;
  let entries;
  try {
    entries = readdirSync(directory, { withFileTypes: true });
  } catch {
    return 0;
  }
  for (const entry of entries) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      newest = Math.max(newest, newestChangeMs(path));
      continue;
    }
    try {
      newest = Math.max(newest, statSync(path).mtimeMs);
    } catch {
      // Raced by a build that replaced the file; the next pass sees it.
    }
  }
  return newest;
}

/**
 * Whether a binary predates the workspace it was built from.
 *
 * A missing binary is stale by definition: that is the case where a run would
 * otherwise fail inside a service starter naming the service rather than the
 * build that never ran.
 */
export function isStaleBinary(binaryPath: string, sourceRoot: string): boolean {
  let builtAtMs: number;
  try {
    builtAtMs = statSync(binaryPath).mtimeMs;
  } catch {
    return true;
  }
  const sourcesMs = Math.max(
    newestChangeMs(join(sourceRoot, "crates")),
    ...WORKSPACE_MANIFESTS.map((name) => {
      try {
        return statSync(join(sourceRoot, name)).mtimeMs;
      } catch {
        return 0;
      }
    }),
  );
  return sourcesMs > builtAtMs;
}

/**
 * Build the binary a smoke knob named, if this checkout can and it is stale.
 *
 * The build runs in the foreground with inherited stdio: cargo's own output is
 * the useful part of a failed mixed-stack run, and a silent background build
 * would leave a spec failing against a binary nobody knows was rebuilt.
 */
export function ensureSmokeStackBinary(binaryPath: string | undefined, sourceRoot: string): void {
  if (!binaryPath) return;
  const profile = cargoProfileFor(binaryPath, sourceRoot);
  if (profile === null) {
    console.log(`smoke stack: ${binaryPath} is not a build of ${sourceRoot}; using it as given`);
    return;
  }
  if (!isStaleBinary(binaryPath, sourceRoot)) return;
  console.log(`smoke stack: building ${CLI_PACKAGE} --bin ${CLI_BINARY} (${profile})`);
  const result = spawnSync(
    process.env.CARGO ?? "cargo",
    [
      "build",
      "--bin",
      CLI_BINARY,
      "-p",
      CLI_PACKAGE,
      ...(profile === "release" ? ["--release"] : []),
    ],
    { cwd: sourceRoot, stdio: "inherit" },
  );
  if (result.error) throw new Error(`cargo build did not start: ${String(result.error)}`);
  if (result.status !== 0) throw new Error(`cargo build exited ${result.status ?? "by signal"}`);
}
