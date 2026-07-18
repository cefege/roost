// `roost test` — run all tests in dep order:
//   1. bun test apps/shared
//   2. bun test apps/coord apps/worker
//   3. bun test apps/web (SPA unit tests — native bun:test)
//   4. Playwright e2e (separate Node runner — see apps/web/playwright.config)
// Stops on first failure.

import { spawn } from "bun";

async function run(cmd: string[], cwd?: string): Promise<number> {
  const p = spawn({ cmd, stdio: ["inherit", "inherit", "inherit"], cwd });
  await p.exited;
  return p.exitCode ?? 1;
}

export async function test(_args: string[]): Promise<void> {
  // Use directory-anchored paths (trailing slash) so bun test filters
  // match only these exact dirs, not substring siblings.
  const steps: Array<[string, () => Promise<number>]> = [
    ["wire spec", () => run(["bun", "test", "apps/shared/tests/"])],
    ["coord", () => run(["bun", "test", "apps/coord/tests/"])],
    ["worker", () => run(["bun", "test", "apps/worker/tests/"])],
    ["web unit", () => run(["bun", "test", "apps/web/tests/"])],
    ["smoke", () => run(["bun", "test", "smoke/"])],
  ];
  for (const [name, fn] of steps) {
    console.log(`>> ${name}`);
    const code = await fn();
    if (code !== 0) {
      console.error(`<< ${name} FAILED (exit ${code})`);
      process.exit(code);
    }
  }
  console.log("all tests green");
}
