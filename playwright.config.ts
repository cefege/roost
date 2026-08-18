import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "smoke/terminal",
  // Every test already owns a fresh browser context, and every WORKER owns a
  // full hermetic stack (its own coord on an ephemeral port, worker, keeper,
  // and PTYs under an mkdtemp root — smoke/terminal/stack.ts). Nothing is
  // shared but the read-only apps/web/dist bundle, so tests are independent
  // both across files and inside a file: parallelize at test granularity, or
  // terminal.spec.ts's 40 cases serialize behind one browser for 6+ minutes.
  fullyParallel: true,
  // 4 on an 8-core host. A worker is not one process: it drives Chromium (plus
  // its renderer and GPU process) against a coord, a worker, a keeper, and the
  // PTY children of every session it spawns, and the flood cases push 20k rows
  // through that whole pipeline, so one worker's peak demand is ~2 cores.
  // 4 x 2 saturates the box exactly; 8 would starve the renderer and convert
  // paint polls into timeouts that assert nothing about the contract they
  // defend. It is also what Playwright's own default (half the logical cores)
  // picks here — pinned rather than inferred so a bigger CI box cannot quietly
  // scale past what one stack per worker can afford in RAM.
  workers: 4,
  // Deliberately 0: a retry would paper over exactly the order- and
  // contention-dependent flakiness that raising parallelism can introduce.
  retries: 0,
  timeout: 120_000,
  expect: { timeout: 10_000 },
  // Both reporters are parallel-safe and keep failures attributable: `list`
  // labels every result line with file:line:title, and its failure block is
  // emitted after the workers drain, while `html` keeps each test's stdout,
  // trace, and video filed under that test rather than in one shared stream.
  reporter: [["list"], ["html", { open: "never" }]],
  outputDir: "test-results/terminal",
  use: {
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  // webkit-iphone is macOS-only: Linux WebKit is a different engine build, and
  // the two tests that need it (smoke/terminal/terminal.spec.ts:64,:129) already
  // self-skip on any other project. Gating here rather than via a CI --project
  // flag keeps `bun run test:terminal` correct on a Linux dev box too.
  // Two passes, run back to back by `roost test terminal`. Correctness cases
  // fan out across workers; anything tagged @serial (the perf/latency cases)
  // runs afterwards with --workers=1 on an otherwise idle box, because a
  // throughput or paint-latency number measured against three other stacks is
  // not a measurement. Keeping the split here rather than in the runner means a
  // bare `bunx playwright test` is still correct: it just runs both projects.
  projects: process.platform === "darwin"
    ? [
      { name: "chromium-desktop", grepInvert: /@serial/, use: { ...devices["Desktop Chrome"] } },
      { name: "webkit-iphone", grepInvert: /@serial/, use: { ...devices["iPhone 15"] } },
      { name: "chromium-serial", grep: /@serial/, fullyParallel: false, use: { ...devices["Desktop Chrome"] } },
    ]
    : [
      { name: "chromium-desktop", grepInvert: /@serial/, use: { ...devices["Desktop Chrome"] } },
      { name: "chromium-serial", grep: /@serial/, fullyParallel: false, use: { ...devices["Desktop Chrome"] } },
    ],
});
