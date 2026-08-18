import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { scrubBootstrapTokenFromServiceDefinition } from "../src/install.ts";

const roots: string[] = [];
const originalPath = process.env.PATH;

afterEach(() => {
  process.env.PATH = originalPath;
  for (const root of roots.splice(0)) rmSync(root, { recursive: true, force: true });
});

describe("bootstrap token service scrub", () => {
  test("atomically removes a redeemed token from a macOS LaunchAgent", async () => {
    const root = mkdtempSync(join(tmpdir(), "roost-token-mac-"));
    roots.push(root);
    const plist = join(root, "worker.plist");
    writeFileSync(plist, [
      "<plist><dict><key>EnvironmentVariables</key><dict>",
      "<key>ROOST_BOOTSTRAP_TOKEN</key><string>one-shot&amp;secret</string>",
      "<key>ROOST_WORKER_LABEL</key><string>worker</string>",
      "</dict></dict></plist>",
    ].join("\n"), { mode: 0o644 });

    expect(await scrubBootstrapTokenFromServiceDefinition(plist, "darwin")).toBe(true);
    const updated = readFileSync(plist, "utf8");
    expect(updated).not.toContain("ROOST_BOOTSTRAP_TOKEN");
    expect(updated).toContain("ROOST_WORKER_LABEL");
    expect(statSync(plist).mode & 0o777).toBe(0o600);
  });

  test("removes the token from systemd and reloads the user manager", async () => {
    const root = mkdtempSync(join(tmpdir(), "roost-token-linux-"));
    roots.push(root);
    const unit = join(root, "worker.service");
    const marker = join(root, "reloaded");
    const systemctl = join(root, "systemctl");
    writeFileSync(unit, [
      "[Service]",
      "Environment=ROOST_BOOTSTRAP_TOKEN=one-shot-secret",
      "Environment=ROOST_WORKER_LABEL=worker",
      "",
    ].join("\n"), { mode: 0o644 });
    writeFileSync(systemctl, `#!/bin/sh\ntouch '${marker}'\n`, { mode: 0o700 });
    chmodSync(systemctl, 0o700);
    process.env.PATH = `${root}:${originalPath ?? "/usr/bin:/bin"}`;

    expect(await scrubBootstrapTokenFromServiceDefinition(unit, "linux")).toBe(true);
    const updated = readFileSync(unit, "utf8");
    expect(updated).not.toContain("ROOST_BOOTSTRAP_TOKEN");
    expect(updated).toContain("ROOST_WORKER_LABEL");
    // The fake systemctl on PATH must actually have run: Bun.file(marker).size
    // is 0 for a missing file, so only existsSync proves the reload was spawned.
    expect(existsSync(marker)).toBe(true);
    expect(statSync(unit).mode & 0o777).toBe(0o600);
  });
});
