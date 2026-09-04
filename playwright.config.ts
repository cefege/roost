import { availableParallelism } from "node:os";
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "smoke/terminal",
  // Every test already owns a fresh browser context, and every WORKER owns a
  // full hermetic stack (its own coord on an ephemeral port, worker, keeper,
  // and PTYs under an mkdtemp root — smoke/terminal/stack.ts). Nothing is
  // shared but the read-only apps/web/dist bundle, so tests are independent
  // both across files and inside a file: parallelize at test granularity, or a
  // themed spec's cases serialize behind one browser for minutes at a time.
  fullyParallel: true,
  // A worker is not one process: it drives Chromium (plus its renderer and GPU
  // process) against a coord, a worker, a keeper, and the PTY children of every
  // session it spawns, and the flood cases push 20k rows through that whole
  // pipeline, so one worker's peak demand is ~2 cores. Size to the host instead
  // of pinning 4: an 8-core dev box still gets 4, while GitHub's Linux runners
  // get 2 rather than 4x oversubscription. macOS CI runs one stack because each
  // correctness pass also carries WebKit and current hosted runners showed
  // unrelated PTY, navigation, and paint timeouts when two stacks competed.
  // Linux CI still exercises the parallel-independence contract.
  workers: process.env.CI && process.platform === "darwin"
    ? 1
    : Math.max(2, Math.min(4, Math.floor(availableParallelism() / 2))),
  // Deliberately 0: a retry would paper over exactly the order- and
  // contention-dependent flakiness that raising parallelism can introduce.
  retries: 0,
  timeout: 120_000,
  // The default budget for a state that must BECOME true (focus lands, a mic
  // enters listening, a marker paints). 10s is ample on a dev box; CI's 3-4 core
  // runners are slow enough that it failed transitions that then passed on
  // rerun, so they get 20s. This does not loosen any timeliness assertion: the
  // specs that measure latency pass their own explicit budget (perf.spec.ts, and
  // terminal-pty-recovery's 10s painted-marker and cursor-geometry proofs).
  expect: { timeout: process.env.CI ? 20_000 : 10_000 },
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
  // the mobile-composer tests that need it already self-skip on any other
  // project, so the skip — not this list — is what keeps them honest. Gating
  // here rather than via a CI --project flag keeps `bun run test:terminal`
  // correct on a Linux dev box too.
  // Two passes, run back to back by `roost test terminal`. Correctness cases
  // fan out across workers; anything tagged @serial (the perf/latency cases)
  // runs afterwards with --workers=1 on an otherwise idle box, because a
  // throughput or paint-latency number measured against three other stacks is
  // not a measurement. Keeping the split here rather than in the runner means a
  // bare `bunx playwright test` is still correct: it just runs both projects.
  projects: process.platform === "darwin"
    ? [
      { name: "chromium-desktop", grepInvert: /@serial/, use: { ...devices["Desktop Chrome"], userAgent: undefined } },
      { name: "webkit-iphone", grepInvert: /@serial/, use: { ...devices["iPhone 15"] } },
      { name: "chromium-serial", grep: /@serial/, fullyParallel: false, use: { ...devices["Desktop Chrome"], userAgent: undefined } },
    ]
    : [
      { name: "chromium-desktop", grepInvert: /@serial/, use: { ...devices["Desktop Chrome"], userAgent: undefined } },
      { name: "chromium-serial", grep: /@serial/, fullyParallel: false, use: { ...devices["Desktop Chrome"], userAgent: undefined } },
    ],
});
