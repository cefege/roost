// Linux prepare must reject an installed worker whose exact unit snapshot lacks
// a full prior build identity. The remote command fails before publishing a
// deploy journal, so later mutation cannot create an unprovable rollback.

import { existsSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import { expect, test } from "bun:test";
import { _linuxPrepareDeployJournalCommand } from "../src/linux-deploy-journal-commands.ts";
import { linuxWorkerReleaseRoot } from "../src/linux-deploy-journal.ts";
import { KEEPER_UPDATE, SHA } from "./deploy-linux-recovery-fixture.ts";

test.skipIf(process.platform !== "linux")("Linux prepare rejects a prior unit without a full build SHA", () => {
  const home = mkdtempSync(join(tmpdir(), "roost-linux-prior-"));
  const tools = join(home, "tools");
  const unitPath = join(home, ".config/systemd/user/roost-worker.service");
  const journalPath = join(home, ".roost/transactions/worker-deploy-journal");
  const targetReleasePath = join(
    linuxWorkerReleaseRoot(home),
    `${SHA}-00000000-0000-4000-8000-000000000001`,
  );
  mkdirSync(tools, { recursive: true });
  mkdirSync(dirname(unitPath), { recursive: true });
  mkdirSync(dirname(journalPath), { recursive: true });
  mkdirSync(targetReleasePath, { recursive: true });
  writeFileSync(unitPath, "[Service]\nWorkingDirectory=/srv/worker\n", { mode: 0o644 });
  const systemctlLog = join(home, "systemctl.log");
  const systemctl = join(tools, "systemctl");
  writeFileSync(systemctl, `#!/bin/sh\nprintf '%s\\n' "$*" >> '${systemctlLog}'\ncase "$*" in\n  *ActiveState*) echo inactive;;\n  *MainPID*) echo 0;;\n  *is-enabled*) echo enabled;;\nesac\n`, { mode: 0o755 });
  const command = _linuxPrepareDeployJournalCommand({
    journalPath,
    unitPath,
    targetSha: SHA,
    targetReleasePath,
    home,
    rolloutId: null,
    workerFingerprint: "f".repeat(64),
    keeperUpdate: KEEPER_UPDATE,
  });
  try {
    const result = Bun.spawnSync(["bash", "-c", command], {
      env: { ...process.env, PATH: `${tools}:${process.env.PATH ?? "/usr/bin:/bin"}` },
    });
    expect(result.exitCode).not.toBe(0);
    expect(existsSync(journalPath)).toBe(false);
    expect(existsSync(`${journalPath}.new`)).toBe(false);
  } finally {
    rmSync(home, { recursive: true, force: true });
  }
});
