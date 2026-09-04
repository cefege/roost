// `roost test <profile>` — canonical local and CI test entry points.
// Profiles keep hermetic unit/terminal coverage, root-owned managed
// qualification, and the live-tailnet API canary behind distinct prerequisites.

import { spawn } from "bun";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const PROFILES = ["unit", "worker", "terminal", "managed", "live-api", "all"] as const;
type TestProfile = (typeof PROFILES)[number];
const PLAYWRIGHT_CLI = "node_modules/@playwright/test/cli.js";
const MANAGED_E2E_GIT_SHA_ZERO = "0".repeat(40);
const MANAGED_IMAGE_ID_RE = /^sha256:[0-9a-f]{64}$/;
const MANAGED_E2E_FILES = [
  "apps/roost-cli/tests/managed-browser.e2e.test.ts",
  "apps/roost-cli/tests/saas-provisioning.e2e.test.ts",
  "apps/roost-cli/tests/saas-backup-restore.e2e.test.ts",
  "apps/roost-cli/tests/saas-open-signup.e2e.test.ts",
] as const;

interface CapturedCommand {
  exitCode: number;
  stdout: string;
  stderr: string;
}

interface ManagedProfileDependencies {
  platform: NodeJS.Platform;
  getuid(): number | undefined;
  which(name: string): string | null;
  chromiumExecutablePath(): Promise<string>;
  pathExists(path: string): boolean;
  createTempRoot(): string;
  removeTempRoot(path: string): void;
  uniqueSuffix(): string;
  command(cmd: string[]): CapturedCommand;
  run(name: string, cmd: string[], env?: Record<string, string>): Promise<void>;
}

async function run(name: string, cmd: string[], env?: Record<string, string>): Promise<void> {
  console.log(`>> ${name}`);
  const process = spawn({
    cmd,
    env: env ? { ...globalThis.process.env, ...env } : undefined,
    stdio: ["inherit", "inherit", "inherit"],
  });
  const exitCode = await process.exited;
  if (exitCode !== 0) throw new Error(`${name} failed (exit ${exitCode ?? 1})`);
}

function captureCommand(cmd: string[]): CapturedCommand {
  const result = Bun.spawnSync(cmd, { stdout: "pipe", stderr: "pipe" });
  return {
    exitCode: result.exitCode,
    stdout: result.stdout.toString().trim(),
    stderr: result.stderr.toString().trim(),
  };
}

function managedE2eGitSha(env: NodeJS.ProcessEnv): string {
  const configured = env.ROOST_SAAS_E2E_GIT_SHA;
  if (configured === undefined) return MANAGED_E2E_GIT_SHA_ZERO;
  if (!/^[0-9a-f]{40}$/i.test(configured)) {
    throw new Error(
      "managed test profile requires ROOST_SAAS_E2E_GIT_SHA to be exactly 40 hexadecimal characters",
    );
  }
  return configured.toLowerCase();
}

function commandFailure(result: CapturedCommand): string {
  return result.stderr || result.stdout || `exit ${result.exitCode}`;
}

async function assertManagedPrerequisites(deps: ManagedProfileDependencies): Promise<void> {
  if (deps.platform !== "linux") throw new Error("managed test profile requires Linux");
  if (deps.getuid() !== 0) throw new Error("managed test profile requires root");
  for (const executable of ["docker", "age", "age-keygen"]) {
    if (!deps.which(executable)) throw new Error(`managed test profile requires ${executable}`);
  }
  const dockerInfo = deps.command(["docker", "info", "--format", "{{.ServerVersion}}"]);
  if (dockerInfo.exitCode !== 0) {
    throw new Error(`managed test profile requires a reachable Docker daemon: ${commandFailure(dockerInfo)}`);
  }
  let chromiumPath: string;
  try {
    chromiumPath = await deps.chromiumExecutablePath();
  } catch {
    throw new Error("managed test profile requires Playwright Chromium; run bunx playwright install chromium");
  }
  if (!chromiumPath || !deps.pathExists(chromiumPath)) {
    throw new Error("managed test profile requires Playwright Chromium; run bunx playwright install chromium");
  }
}

function systemManagedProfileDependencies(): ManagedProfileDependencies {
  return {
    platform: process.platform,
    getuid: () => typeof process.getuid === "function" ? process.getuid() : undefined,
    which: (name) => Bun.which(name),
    // Probe in a child process so production compilation never follows Playwright.
    chromiumExecutablePath: async () => {
      const result = captureCommand([
        process.execPath, "-e",
        "import { chromium } from '@playwright/test'; process.stdout.write(chromium.executablePath())",
      ]);
      if (result.exitCode !== 0) throw new Error(commandFailure(result));
      return result.stdout;
    },
    pathExists: existsSync,
    createTempRoot: () => mkdtempSync(join(tmpdir(), "roost-managed-profile-")),
    removeTempRoot: (path) => rmSync(path, { recursive: true, force: true }),
    uniqueSuffix: () => `${process.pid}-${crypto.randomUUID().replaceAll("-", "").slice(0, 12)}`,
    command: captureCommand,
    run,
  };
}

function recordCleanupFailure(
  failures: string[],
  label: string,
  result: CapturedCommand,
): void {
  if (result.exitCode !== 0) failures.push(`${label}: ${commandFailure(result)}`);
}

async function runManagedProfile(
  env: NodeJS.ProcessEnv = process.env,
  deps: ManagedProfileDependencies = systemManagedProfileDependencies(),
): Promise<void> {
  await assertManagedPrerequisites(deps);
  const gitSha = managedE2eGitSha(env);

  const tempRoot = deps.createTempRoot();
  const suffix = deps.uniqueSuffix();
  const imageTag = `roost-managed-e2e:${suffix}`;
  const network = `roost-managed-e2e-${suffix}`;
  const imageIdFile = join(tempRoot, "coordinator-image.id");
  let imageTagOwned = false;
  let networkOwned = false;
  let profileFailed = false;
  let profileFailure: unknown;

  try {
    await deps.run("managed coordinator image", [
      "docker",
      "build",
      "--iidfile",
      imageIdFile,
      "--tag",
      imageTag,
      "--build-arg",
      "ROOST_VERSION=0.0.0-managed-e2e",
      "--build-arg",
      `ROOST_GIT_SHA=${gitSha}`,
      "--file",
      "Dockerfile.coord",
      ".",
    ]);
    imageTagOwned = true;
    const imageId = readFileSync(imageIdFile, "utf8").trim();
    if (!MANAGED_IMAGE_ID_RE.test(imageId)) {
      throw new Error("managed coordinator build did not export an immutable image ID");
    }
    const inspectedImage = deps.command(["docker", "image", "inspect", "--format", "{{.Id}}", imageTag]);
    if (inspectedImage.exitCode !== 0) {
      throw new Error(`managed coordinator image inspection failed: ${commandFailure(inspectedImage)}`);
    }
    if (inspectedImage.stdout !== imageId) {
      throw new Error("managed coordinator image tag does not resolve to the exported immutable image ID");
    }

    await deps.run("managed Docker network", [
      "docker",
      "network",
      "create",
      "--label",
      `com.roost.test-managed=${suffix}`,
      network,
    ]);
    networkOwned = true;
    await deps.run("managed", [process.execPath, "test", ...MANAGED_E2E_FILES], {
      ROOST_SAAS_E2E: "1",
      ROOST_SIGNUP_E2E: "1",
      ROOST_SAAS_E2E_IMAGE: imageId,
      ROOST_SAAS_E2E_NETWORK: network,
      ROOST_SAAS_E2E_GIT_SHA: gitSha,
    });
  } catch (error) {
    profileFailed = true;
    profileFailure = error;
  }

  const cleanupFailures: string[] = [];
  if (networkOwned) {
    recordCleanupFailure(
      cleanupFailures,
      "managed Docker network cleanup",
      deps.command(["docker", "network", "rm", network]),
    );
  }
  if (imageTagOwned) {
    recordCleanupFailure(
      cleanupFailures,
      "managed coordinator image cleanup",
      deps.command(["docker", "image", "rm", imageTag]),
    );
  }
  try {
    deps.removeTempRoot(tempRoot);
  } catch (error) {
    cleanupFailures.push(`managed temporary directory cleanup: ${String(error)}`);
  }

  if (profileFailed) {
    for (const failure of cleanupFailures) console.error(`>> ${failure}`);
    throw profileFailure;
  }
  if (cleanupFailures.length > 0) throw new Error(cleanupFailures.join("\n"));
}

async function runUnit(): Promise<void> {
  await run("worker", [process.execPath, "scripts/test-worker.ts"]);
  // --isolate: a fresh global object per file. `bun test` otherwise shares one
  // process, so a test that installs a fake DOM global or calls mock.module
  // (both deliberate here — this repo runs no jsdom) silently poisons every
  // file that happens to run after it. That made apps/web failures a function
  // of suite order: a partial `document` stub crashed pageVisible.ts at module
  // eval, and a mocked store made transfers.test.ts fail to import `transfers`.
  // mock.module cannot be reliably undone in-process (a re-mock with the real
  // namespace leaves the mocked keys in place, measured on bun 1.3.14), so
  // isolation is the fix rather than per-file cleanup.
  // --timeout: Bun's 5 s default is a logic budget, but this profile runs 160
  // files whose slowest cases are I/O-bound (sqlite WAL backups, real archive
  // copies, ~10 `bash -lc` login shells per deploy-lock case). Under that
  // contention they overran the default and failed as timeouts while asserting
  // nothing about the contract they defend. 30 s is still short enough that a
  // genuine hang fails the run rather than parking CI.
  await run("unit", [
    process.execPath,
    "test",
    "--isolate",
    "--timeout",
    "30000",
    "apps/shared/tests/",
    "apps/coord/tests/",
    "apps/web/tests/",
    "apps/web/src/",
    "apps/roost-cli/tests/",
    "smoke/bun_smoke.test.ts",
  ], {
    // Web sources gate smoke hooks behind VITE_ROOST_SMOKE at build time; unit
    // tests arm them via the runtime flag alone, so declare the build flag here too.
    VITE_ROOST_SMOKE: "1",
  });
}

async function runTerminal(): Promise<void> {
  try {
    // VITE_ROOST_SMOKE=1 bakes the window.__smoke backdoor chunk into dist; the SPA
    // still arms it only behind localStorage.roostSmoke (smoke fixtures set it pre-boot).
    await run("web build", [process.execPath, "run", "--cwd", "apps/web", "build"], {
      VITE_ROOST_SMOKE: "1",
    });
    await run("web embed", [process.execPath, "scripts/gen-embed.ts"]);
    // Pass 1: correctness, fanned out (playwright.config.ts pins workers:4).
    await run(
      "terminal",
      [
        process.execPath, PLAYWRIGHT_CLI, "test", "--config=playwright.config.ts",
        ...(process.platform === "darwin"
          ? ["--project=chromium-desktop", "--project=webkit-iphone"]
          : ["--project=chromium-desktop"]),
      ],
      { ROOST_TEST_BUN: process.execPath },
    );
    // Pass 2: the @serial (perf/latency) cases, alone on the box. A number
    // measured under the other three stacks' load asserts nothing.
    await run(
      "terminal perf",
      [
        process.execPath, PLAYWRIGHT_CLI, "test", "--config=playwright.config.ts",
        "--project=chromium-serial", "--workers=1",
      ],
      { ROOST_TEST_BUN: process.execPath },
    );
  } finally {
    await run("restore embed stubs", [process.execPath, "scripts/gen-embed.ts", "--stub"]);
  }
}

export async function test(args: string[]): Promise<void> {
  const profile = args[0] ?? "all";
  if (args.length > 1 || !PROFILES.includes(profile as TestProfile)) {
    throw new Error(`unknown test profile "${profile}"; valid profiles: ${PROFILES.join(", ")}`);
  }

  switch (profile as TestProfile) {
    case "unit":
      await runUnit();
      return;
    case "worker":
      await run("worker", [process.execPath, "scripts/test-worker.ts"]);
      return;
    case "terminal":
      await runTerminal();
      return;
    case "managed":
      await runManagedProfile();
      return;
    case "live-api":
      if (!process.env.ROOST_COORD_URL) {
        throw new Error(
          "live-api requires ROOST_COORD_URL; run ROOST_COORD_URL=https://<current-tailnet-coord>:4102 bun run test:live-api",
        );
      }
      await run("live-api", [
        process.execPath, "test",
        "smoke/api_smoke.test.ts",
      ]);
      return;
    case "all":
      await runUnit();
      await runTerminal();
  }
}

export const _managedTestInternals = {
  assertManagedPrerequisites,
  managedE2eGitSha,
  runManagedProfile,
};
