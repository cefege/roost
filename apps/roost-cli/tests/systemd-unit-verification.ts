// Portable systemd unit syntax verification for deployment asset tests.
// It substitutes only the unavailable installed binary in temporary copies;
// callers separately assert the production ExecStart command and arguments.

import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";

export function verifySystemdUnitSyntax(systemdAnalyze: string, unitPaths: string[]): number {
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "roost-systemd-verify-"));
  try {
    const temporaryUnits = unitPaths.map((unitPath) => {
      const temporaryPath = join(temporaryDirectory, basename(unitPath));
      const unit = readFileSync(unitPath, "utf8").replaceAll("/usr/local/bin/roost", "/bin/true");
      writeFileSync(temporaryPath, unit);
      return temporaryPath;
    });
    return Bun.spawnSync([systemdAnalyze, "verify", ...temporaryUnits]).exitCode;
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
}
