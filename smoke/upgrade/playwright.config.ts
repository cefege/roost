// Playwright config for the upgrade-continuity tier. Separate from the terminal
// config because these tests are not parallel-safe by construction: each stages
// git worktrees of other releases out of this checkout and drives one install
// through a real deploy. `roost test upgrade` runs it; nothing else should.

import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  // One worker, deliberately. `git worktree add` and `git worktree remove`
  // mutate this checkout's shared administrative state, and a deploy replaces
  // the one worker process an install has. Both serialize anyway.
  workers: 1,
  fullyParallel: false,
  // Deliberately 0: an upgrade that only survives on the second attempt has
  // not survived, and a retry would hide exactly that.
  retries: 0,
  // A run stages two releases, boots a released coordinator, migrates its
  // database forward, opens two PTYs, deploys, and then waits for the keeper
  // proof a 30s worker heartbeat carries. The budget is dominated by those
  // waits rather than by anything this tier computes.
  timeout: 600_000,
  expect: { timeout: process.env.CI ? 20_000 : 10_000 },
  // Inside test-results/, which .gitignore already covers, and a sibling of
  // outputDir so the HTML reporter does not clash with the trace output.
  reporter: [["list"], ["html", { open: "never", outputFolder: "../../test-results/upgrade-report" }]],
  outputDir: "../../test-results/upgrade",
  use: {
    trace: "retain-on-failure",
    video: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  // Chromium only: the subject is the deploy and keeper handoff, and the
  // browser is here to prove the PTY markers survived. Engine coverage for the
  // renderer itself belongs to the terminal tier.
  projects: [
    {
      name: "chromium-desktop",
      use: { ...devices["Desktop Chrome"], userAgent: undefined },
    },
  ],
});
