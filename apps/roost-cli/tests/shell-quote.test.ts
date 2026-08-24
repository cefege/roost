// Byte-stability proof for the one POSIX shell quoter every roost-cli remote
// command goes through. The journal/lock/bootstrap command strings built on
// top of it are compared byte-for-byte by recovery tooling, so these tests
// pin both the exact escaping bytes AND the semantic contract (round-trip
// through bash/sh without interpretation) that the deploy paths rely on.

import { describe, expect, test } from "bun:test";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { posixShellQuote } from "../src/shell-quote.ts";

describe("posixShellQuote", () => {
  // Caller shapes: deploy-linux journal specs, deploy-exec lock paths,
  // service-ctl launchd labels/plists all pass POSIX filesystem strings.
  const callerCases = [
    "/home/ubuntu/.local/share/roost/releases/worker/worker-deploy-journal",
    "$HOME/RoostWorkerV2",
    "/tmp/space dir/plist path.plist",
    "com.roost.worker-v2",
    "it's",
    "back`tick`and$(substituted)",
    "semi;colon|pipe&amp",
    "*glob[chars]?",
    'double"quotes',
    "line1\nline2",
    "",
  ];

  test("escapes to the canonical close-quote-escape-open bytes", () => {
    expect(posixShellQuote("it's")).toBe("'it'\"'\"'s'");
    expect(posixShellQuote("plain")).toBe("'plain'");
  });

  for (const value of callerCases) {
    test(`bash round-trips ${JSON.stringify(value)}`, () => {
      const root = mkdtempSync(join(tmpdir(), "roost-shellquote-"));
      try {
        const probe = join(root, "probe");
        const proc = Bun.spawnSync([
          "bash",
          "-c",
          `VALUE=${posixShellQuote(value)}; printf '%s' "$VALUE" > ${posixShellQuote(probe)}`,
        ]);
        expect(proc.exitCode).toBe(0);
        expect(Bun.file(probe).text()).resolves.toBe(value);
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    });
  }

  test("sh (dash) round-trips the same bytes as bash", () => {
    const value = "mix 'single' \"double\" $(x) `y` $VAR";
    const proc = Bun.spawnSync(["sh", "-c", `printf '%s' ${posixShellQuote(value)}`]);
    expect(proc.exitCode).toBe(0);
    expect(proc.stdout.toString()).toBe(value);
  });

  test("quoted values never execute metacharacters", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-shellquote-"));
    const marker = join(root, "pwned");
    try {
      const payload = `$(touch ${marker})`;
      const proc = Bun.spawnSync(["sh", "-c", `printf '%s' ${posixShellQuote(payload)}`]);
      expect(proc.exitCode).toBe(0);
      expect(proc.stdout.toString()).toBe(payload);
      expect(existsSync(marker)).toBe(false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});
