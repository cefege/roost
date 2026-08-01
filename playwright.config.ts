import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "smoke/terminal",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 120_000,
  expect: { timeout: 10_000 },
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
  projects: process.platform === "darwin"
    ? [
      { name: "chromium-desktop", use: { ...devices["Desktop Chrome"] } },
      { name: "webkit-iphone", use: { ...devices["iPhone 15"] } },
    ]
    : [{ name: "chromium-desktop", use: { ...devices["Desktop Chrome"] } }],
});
