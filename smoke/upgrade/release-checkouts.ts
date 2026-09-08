// Stages the releases an upgrade run needs: the commit the install already
// runs, and the tagged release whose coordinator database the working tree must
// still open. Each is a real git worktree with its own frozen dependency
// install and its own generated keeper contract, which is exactly what
// `roost deploy` stages. smoke/upgrade/fixtures.ts owns removal; nothing here
// writes to this checkout.

import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { REPOSITORY_ROOT } from "../terminal/stack-runtime.ts";
import type { TerminalReleaseCheckout } from "../terminal/stack.ts";

/** The commit the existing install runs. Default is the parent of the change
 *  under test: that is the release an operator is upgrading FROM in CI. */
const PRIOR_COMMIT_REF_ENV = "ROOST_UPGRADE_PRIOR_REF";
/** The oldest coordinator database the working tree claims to open. */
const RELEASE_TAG_REF_ENV = "ROOST_UPGRADE_RELEASE_REF";

export interface StagedRelease extends TerminalReleaseCheckout {
  remove(): void;
}

export function resolveGitSha(ref: string): string {
  return execFileSync("git", ["rev-parse", ref], {
    cwd: REPOSITORY_ROOT,
    encoding: "utf8",
  }).trim();
}

export function priorCommitRef(): string {
  return process.env[PRIOR_COMMIT_REF_ENV] ?? "HEAD~1";
}

export function releaseTagRef(): string {
  return process.env[RELEASE_TAG_REF_ENV]
    ?? execFileSync("git", ["describe", "--tags", "--abbrev=0", "HEAD"], {
      cwd: REPOSITORY_ROOT,
      encoding: "utf8",
    }).trim();
}

/** `keeperImplementationDigest` stands in for a release whose keeper bundle
 *  changed: gen-embed accepts a supplied digest (that is how compiled artifacts
 *  carry theirs), and a different digest is exactly what makes a keeper
 *  unadoptable by the staged release. */
export function stageRelease(
  ref: string,
  label: string,
  keeperImplementationDigest?: string,
): StagedRelease {
  const gitSha = resolveGitSha(ref);
  const stagingRoot = mkdtempSync(join("/tmp", `roost-upgrade-${label}-`));
  const sourceRoot = join(stagingRoot, "checkout");
  const bunExecutable = process.env.ROOST_TEST_BUN ?? "bun";
  try {
    run("git", ["worktree", "add", "--quiet", "--force", "--detach", sourceRoot, gitSha], REPOSITORY_ROOT);
    // The release's own lockfile, not this checkout's: a release that pinned
    // different dependencies must still run as it shipped.
    run(bunExecutable, ["install", "--frozen-lockfile"], sourceRoot);
    // Stub embeds keep the coordinator serving migrations and the SPA from
    // disk, and the same run writes the keeper implementation digest. Without
    // it the checkout's keeper contract describes whatever tree last generated
    // the committed file, so update admission would compare the wrong builds.
    run(bunExecutable, ["scripts/gen-embed.ts", "--stub"], sourceRoot, keeperImplementationDigest
      ? { ROOST_GENERATED_KEEPER_IMPLEMENTATION_DIGEST: keeperImplementationDigest }
      : undefined);
  } catch (error) {
    removeStagedRelease(stagingRoot, sourceRoot);
    throw error;
  }
  return {
    sourceRoot,
    gitSha,
    remove: () => removeStagedRelease(stagingRoot, sourceRoot),
  };
}

function run(
  command: string,
  args: readonly string[],
  cwd: string,
  env?: Record<string, string>,
): void {
  try {
    execFileSync(command, [...args], {
      cwd,
      stdio: ["ignore", "pipe", "pipe"],
      ...(env ? { env: { ...process.env, ...env } } : {}),
    });
  } catch (error) {
    let detail = String(error);
    if (error && typeof error === "object" && "stderr" in error) {
      const stderr = error.stderr;
      if (stderr instanceof Buffer) detail = stderr.toString("utf8");
      else if (typeof stderr === "string") detail = stderr;
    }
    throw new Error(`${command} ${args.join(" ")} failed in ${cwd}: ${detail.trim() || String(error)}`);
  }
}

function removeStagedRelease(stagingRoot: string, sourceRoot: string): void {
  // Prune the administrative entry before the files: a directory removed behind
  // git's back leaves a stale worktree registration in this checkout.
  try {
    execFileSync("git", ["worktree", "remove", "--force", sourceRoot], {
      cwd: REPOSITORY_ROOT,
      stdio: "ignore",
    });
  } catch {
    execFileSync("git", ["worktree", "prune"], { cwd: REPOSITORY_ROOT, stdio: "ignore" });
  }
  rmSync(stagingRoot, { recursive: true, force: true });
}
