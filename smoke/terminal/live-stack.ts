#!/usr/bin/env bun
// Long-lived real stack (coord + worker + real keeper + real PTYs) for manual and
// agent-driven browser verification — the render-stress harness and the smoke
// flow need a REAL interactive session, not a Playwright-owned one that dies with
// its test. Same startTerminalTestStack the specs use, so the subject under test
// is the working tree, never a deployed build.
//
//   bun smoke/terminal/live-stack.ts        # prints READY <url>, runs until SIGINT
//
// Requires apps/web/dist to be current AND smoke-enabled (the window.__smoke tier):
// `VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`.

import { startTerminalTestStack } from "./stack.ts";

const stack = await startTerminalTestStack();
process.stdout.write(`READY ${stack.baseUrl} worker=${stack.workerFp}\n`);

let stopping = false;
const stop = async (): Promise<void> => {
  if (stopping) return;
  stopping = true;
  await stack.stop().catch((error: unknown) => {
    process.stderr.write(`stack stop failed: ${String(error)}\n`);
  });
  process.exit(0);
};

process.on("SIGINT", () => void stop());
process.on("SIGTERM", () => void stop());
setInterval(() => undefined, 60_000);
