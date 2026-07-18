// Regression: a hanging `tailscale status` MUST NOT wedge worker boot.
// Root cause (2026-07-08): resolveTailnetDnsName's Bun.spawnSync had no
// timeout; early post-login the Tailscale.app CLI shim hangs → boot blocked
// 7.5min → worker never reached CoordLink. Fix bounds the spawn (timeout +
// SIGKILL) so a hung binary falls through to the next / to ROOST_REACHABLE_ADDR.
// This test drives a real hanging binary through the real resolve loop.

import { test, expect } from "bun:test";
import { mkdtempSync, writeFileSync, chmodSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { resolveTailnetDnsName } from "../src/install.ts";

test("resolveTailnetDnsName returns bounded on a hanging binary", () => {
  const dir = mkdtempSync(join(tmpdir(), "roost-tailhang-"));
  const fake = join(dir, "tailscale");
  writeFileSync(fake, "#!/bin/sh\nsleep 30\n");
  chmodSync(fake, 0o755);

  const t0 = performance.now();
  const dns = resolveTailnetDnsName([fake]); // single hanging bin, no fallback
  const ms = performance.now() - t0;

  // 2s spawn timeout (install.ts) + slack; a regression that drops the
  // timeout would block ~30s here.
  expect(ms).toBeLessThan(6000);
  expect(dns).toBe(""); // hung → killed → exitCode null → loop ends → ""
});
