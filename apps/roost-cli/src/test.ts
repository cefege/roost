// `roost test <profile>` — canonical local and CI test entry points.
// Profiles keep hermetic unit/terminal coverage distinct from the live-tailnet
// API canary, whose network prerequisite must never appear as a green skip.

import { spawn } from "bun";

const PROFILES = ["unit", "worker", "terminal", "live-api", "all"] as const;
type TestProfile = (typeof PROFILES)[number];

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
  await run("unit", [
    process.execPath,
    "test",
    "--isolate",
    "apps/shared/tests/",
    "apps/coord/tests/",
    "apps/web/tests/",
    "apps/web/src/",
    "apps/roost-cli/tests/",
    "smoke/bun_smoke.test.ts",
  ]);
}

async function runTerminal(): Promise<void> {
  try {
    await run("web build", [process.execPath, "run", "--cwd", "apps/web", "build"]);
    await run("web embed", [process.execPath, "scripts/gen-embed.ts"]);
    await run(
      "terminal",
      ["bunx", "playwright", "test", "--config=playwright.config.ts"],
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
