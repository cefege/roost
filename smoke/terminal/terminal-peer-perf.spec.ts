// Serial real-stack performance qualification for terminal direct carriers.
// The helper drives trusted browser keys through fixture PTYs and painted ACKs.
// It publishes distributions on every host while gating absolute budgets only on reference hosts.
import { test } from "./fixtures.ts";
import { probeTerminalPeerPerformance } from "./terminal-peer-perf.ts";

test("terminal peer performance compares Sync, loopback, and direct routes @serial", async ({ browser }, testInfo) => {
  test.skip(!testInfo.project.name.startsWith("chromium"), "desktop browser peer performance qualification");
  test.setTimeout(1_800_000);
  await probeTerminalPeerPerformance(browser, testInfo);
});
