// `roost deploy --force-live` authorizes the DEPLOYED worker to discard a keeper
// it cannot prove, so the flag has to survive every hop to that worker process:
// composed install environment, service definition written by install.sh, and
// the worker's own configuration read. It must survive no further than that —
// the activation that spends it leaves nothing for the next deploy to inherit.
// Both definition formats are driven on every host: `uname` is faked, not read.

import { afterEach, expect, test } from "bun:test";
import {
  chmodSync,
  copyFileSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { parsePosixServiceEnvironment } from "../src/deploy-plist-env.ts";
import {
  KEEPER_FORCE_LIVE_RETIRE_ENV,
  workerInstallEnvironment,
} from "../src/deploy-worker-environment.ts";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { scrubServiceDefinitionEnv } from "../../worker/src/service-definition-env.ts";

type DefinitionKind = "darwin" | "linux";

const GIT_SHA = "b".repeat(40);
const COORDINATOR_URL = "https://coord.example.test:4102";
const REACHABLE_ADDR = "mihai-m5-air.tail67850e.ts.net";
const WORKER_LABEL = "mihai-m5-air";
const cleanupRoots: string[] = [];
const originalPath = process.env.PATH;

interface DeployTarget {
  root: string;
  script: string;
  home: string;
  bin: string;
}

/** A staged release on a target whose reported OS is the installer's own
 * `uname`, so the plist branch is exercised on Linux runners and vice versa. */
function stagedDeployTarget(kind: DefinitionKind): DeployTarget {
  const root = mkdtempSync(join(tmpdir(), "roost-force-live-deploy-"));
  cleanupRoots.push(root);
  const script = join(root, "repo", "apps", "worker", "scripts", "install.sh");
  const home = join(root, "home");
  const bin = join(root, "bin");
  mkdirSync(dirname(script), { recursive: true });
  mkdirSync(home, { recursive: true });
  mkdirSync(bin, { recursive: true });
  copyFileSync(resolve(import.meta.dir, "../../worker/scripts/install.sh"), script);
  chmodSync(script, 0o700);
  const reported = kind === "darwin" ? "Darwin" : "Linux";
  writeFileSync(join(bin, "uname"), `#!/usr/bin/env bash\necho ${reported}\n`, { mode: 0o700 });
  chmodSync(join(bin, "uname"), 0o700);
  return { root, script, home, bin };
}

/** Run the installer the way a POSIX deploy does: the composed environment is a
 * shell prefix on the remote `install.sh write-plist` command, so a value the
 * installer never writes into the definition is lost with the shell. */
function installedServiceDefinition(
  target: DeployTarget,
  kind: DefinitionKind,
  passthroughEnv: string,
  name: string,
): Record<string, string> {
  const definitionPath = join(target.root, name);
  const result = Bun.spawnSync(
    ["bash", "-c", `${passthroughEnv} bash ${target.script} write-plist`],
    {
      env: {
        PATH: `${target.bin}:${originalPath ?? "/usr/bin:/bin"}`,
        HOME: target.home,
        BUN_BIN: process.execPath,
        ROOST_WORKER_AGENT_LABEL: "roost-worker-force-live-test",
        ROOST_WORKER_PLIST: definitionPath,
        ROOST_WORKER_UNIT: definitionPath,
        ROOST_WORKER_DATA_DIR: join(target.root, "data"),
        ROOST_WORKER_LOG_DIR: join(target.root, "logs"),
        ROOST_WORKER_MEMORY_HIGH: "3G",
      },
    },
  );
  expect(result.exitCode, result.stderr.toString()).toBe(0);
  return parsePosixServiceEnvironment(readFileSync(definitionPath, "utf8"), kind);
}

function priorInstalledDefinition(kind: DefinitionKind): string {
  return kind === "darwin"
    ? [
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
        "<plist version=\"1.0\"><dict><key>EnvironmentVariables</key><dict>",
        `<key>ROOST_COORDINATOR_URL</key><string>${COORDINATOR_URL}</string>`,
        `<key>ROOST_WORKER_LABEL</key><string>${WORKER_LABEL}</string>`,
        `<key>ROOST_REACHABLE_ADDR</key><string>${REACHABLE_ADDR}</string>`,
        "</dict></dict></plist>",
      ].join("\n")
    : [
        "[Service]",
        `Environment="ROOST_COORDINATOR_URL=${COORDINATOR_URL}"`,
        `Environment="ROOST_WORKER_LABEL=${WORKER_LABEL}"`,
        `Environment="ROOST_REACHABLE_ADDR=${REACHABLE_ADDR}"`,
        "",
      ].join("\n");
}

afterEach(() => {
  process.env.PATH = originalPath;
  for (const root of cleanupRoots.splice(0)) rmSync(root, { recursive: true, force: true });
});

for (const kind of ["darwin", "linux"] as const) {
  test.skipIf(process.platform === "win32")(
    `--force-live reaches the deployed worker through a ${kind} definition that reuses installed values`,
    () => {
      const target = stagedDeployTarget(kind);
      const reused = parsePosixServiceEnvironment(priorInstalledDefinition(kind), kind);
      expect(reused.ROOST_WORKER_LABEL).toBe(WORKER_LABEL);

      const passthroughEnv = workerInstallEnvironment(reused, {
        ROOST_COORDINATOR_URL: COORDINATOR_URL,
        [KEEPER_FORCE_LIVE_RETIRE_ENV]: "1",
      }, GIT_SHA);
      expect(passthroughEnv).toContain(`${KEEPER_FORCE_LIVE_RETIRE_ENV}='1'`);

      const installed = installedServiceDefinition(target, kind, passthroughEnv, "worker-forced");
      expect(installed[KEEPER_FORCE_LIVE_RETIRE_ENV]).toBe("1");
      expect(installed.ROOST_WORKER_LABEL).toBe(WORKER_LABEL);
      expect(installed.ROOST_REACHABLE_ADDR).toBe(REACHABLE_ADDR);
      expect(loadWorkerConfig(installed, kind).keeperForceLiveRetire).toBe(true);
    },
  );

  test.skipIf(process.platform === "win32")(
    `a spent ${kind} authorization leaves nothing for the next deploy to re-arm`,
    async () => {
      const target = stagedDeployTarget(kind);
      const definitionPath = join(target.root, "worker-forced");
      const armed = installedServiceDefinition(target, kind, workerInstallEnvironment({}, {
        ROOST_COORDINATOR_URL: COORDINATOR_URL,
        ROOST_WORKER_LABEL: WORKER_LABEL,
        [KEEPER_FORCE_LIVE_RETIRE_ENV]: "1",
      }, GIT_SHA), "worker-forced");
      expect(armed[KEEPER_FORCE_LIVE_RETIRE_ENV]).toBe("1");

      // A `systemctl --user daemon-reload` against the host user manager is not
      // this test's business; the fake proves the reload was spawned.
      const reloadMarker = join(target.root, "reloaded");
      writeFileSync(join(target.bin, "systemctl"), `#!/bin/sh\ntouch '${reloadMarker}'\n`, { mode: 0o700 });
      chmodSync(join(target.bin, "systemctl"), 0o700);
      process.env.PATH = `${target.bin}:${originalPath ?? "/usr/bin:/bin"}`;
      expect(await scrubServiceDefinitionEnv(KEEPER_FORCE_LIVE_RETIRE_ENV, definitionPath, kind)).toBe(true);
      process.env.PATH = originalPath;

      const spent = parsePosixServiceEnvironment(readFileSync(definitionPath, "utf8"), kind);
      expect(Object.hasOwn(spent, KEEPER_FORCE_LIVE_RETIRE_ENV)).toBe(false);
      expect(spent.ROOST_COORDINATOR_URL).toBe(COORDINATOR_URL);
      expect(spent.ROOST_WORKER_LABEL).toBe(WORKER_LABEL);
      expect(loadWorkerConfig(spent, kind).keeperForceLiveRetire).toBe(false);

      const nextDeployEnv = workerInstallEnvironment(spent, {
        ROOST_COORDINATOR_URL: COORDINATOR_URL,
      }, GIT_SHA);
      expect(nextDeployEnv).not.toContain(KEEPER_FORCE_LIVE_RETIRE_ENV);
      const reinstalled = installedServiceDefinition(target, kind, nextDeployEnv, "worker-next");
      expect(loadWorkerConfig(reinstalled, kind).keeperForceLiveRetire).toBe(false);

      // Second layer: a definition still carrying the authorization — a worker
      // that died before spending it — is stripped by the next deploy anyway.
      expect(workerInstallEnvironment({ ...spent, [KEEPER_FORCE_LIVE_RETIRE_ENV]: "1" }, {
        ROOST_COORDINATOR_URL: COORDINATOR_URL,
      }, GIT_SHA)).not.toContain(KEEPER_FORCE_LIVE_RETIRE_ENV);
    },
  );
}
