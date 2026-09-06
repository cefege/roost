// Worker-service environment propagation for opt-in OMP conversation restore.
// This covers first installation, installed-value precedence, and the embedded
// POSIX binary installer without entering worker boot or reconciliation.

import { afterEach, describe, expect, test } from "bun:test";
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
  workerInstallEnvironment,
  workerInstallEnvironmentValues,
} from "../src/deploy-worker-environment.ts";
import { installWorkerAgent } from "../src/install-binary-agents.ts";

const GIT_SHA = "a".repeat(40);
const originalRestore = process.env.ROOST_AGENT_CONVERSATION_RESTORE;
const cleanupRoots: string[] = [];

afterEach(() => {
  if (originalRestore === undefined) delete process.env.ROOST_AGENT_CONVERSATION_RESTORE;
  else process.env.ROOST_AGENT_CONVERSATION_RESTORE = originalRestore;
  for (const root of cleanupRoots.splice(0)) {
    rmSync(root, { recursive: true, force: true });
  }
});

describe("worker deploy conversation-restore environment", () => {
  test("forwards a first-install invocation override", () => {
    process.env.ROOST_AGENT_CONVERSATION_RESTORE = "1";
    const environment = workerInstallEnvironment({}, {}, GIT_SHA);

    expect(environment).toContain("ROOST_AGENT_CONVERSATION_RESTORE='1'");
  });

  test("carries invalid explicit ambient values through to boot validation", () => {
    for (const value of ["", "2"]) {
      const environment = workerInstallEnvironmentValues({}, {}, GIT_SHA, {
        ROOST_AGENT_CONVERSATION_RESTORE: value,
      });
      expect(environment.ROOST_AGENT_CONVERSATION_RESTORE).toBe(value);
    }
  });

  test("preserves an installed explicit enable over ambient state", () => {
    process.env.ROOST_AGENT_CONVERSATION_RESTORE = "0";
    const environment = workerInstallEnvironment({
      ROOST_AGENT_CONVERSATION_RESTORE: "1",
    }, {}, GIT_SHA);

    expect(environment).toContain("ROOST_AGENT_CONVERSATION_RESTORE='1'");
    expect(environment).not.toContain("ROOST_AGENT_CONVERSATION_RESTORE='0'");
  });

  test("keeps an installed explicit opt-out across later deploys", () => {
    process.env.ROOST_AGENT_CONVERSATION_RESTORE = "1";
    const environment = workerInstallEnvironment({
      ROOST_AGENT_CONVERSATION_RESTORE: "0",
    }, {}, GIT_SHA);

    expect(environment).toContain("ROOST_AGENT_CONVERSATION_RESTORE='0'");
    expect(environment).not.toContain("ROOST_AGENT_CONVERSATION_RESTORE='1'");
  });

  test.skipIf(process.platform === "win32")("canonical POSIX reinstall preserves an explicit opt-out", () => {
    const root = mkdtempSync(join(tmpdir(), "roost-worker-restore-install-"));
    cleanupRoots.push(root);
    const script = join(root, "repo", "apps", "worker", "scripts", "install.sh");
    const home = join(root, "home");
    const definitionPath = join(root, process.platform === "darwin" ? "worker.plist" : "worker.service");
    mkdirSync(dirname(script), { recursive: true });
    mkdirSync(home, { recursive: true });
    copyFileSync(
      resolve(import.meta.dir, "../../worker/scripts/install.sh"),
      script,
    );
    chmodSync(script, 0o700);
    writeFileSync(definitionPath, process.platform === "darwin"
      ? [
          "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
          "<plist version=\"1.0\"><dict><key>EnvironmentVariables</key><dict>",
          "<key>ROOST_AGENT_CONVERSATION_RESTORE</key><string>0</string>",
          "</dict></dict></plist>",
        ].join("\n")
      : [
          "[Service]",
          "Environment=\"ROOST_AGENT_CONVERSATION_RESTORE=0\"",
          "",
        ].join("\n"));
    const environment: Record<string, string | undefined> = {
      ...process.env,
      HOME: home,
      BUN_BIN: process.execPath,
      GIT_SHA,
      ROOST_COORDINATOR_URL: "https://coord.example.test:4102",
      ROOST_WORKER_AGENT_LABEL: "roost-worker-restore-test",
      ROOST_WORKER_PLIST: definitionPath,
      ROOST_WORKER_UNIT: definitionPath,
      ROOST_WORKER_DATA_DIR: join(root, "data"),
      ROOST_WORKER_LOG_DIR: join(root, "logs"),
      ROOST_WORKER_MEMORY_HIGH: "3G",
    };

    const result = Bun.spawnSync(["bash", script, "write-plist"], { env: environment });
    expect(result.exitCode, result.stderr.toString()).toBe(0);
    const platform = process.platform === "darwin" ? "darwin" : "linux";
    expect(parsePosixServiceEnvironment(readFileSync(definitionPath, "utf8"), platform))
      .toMatchObject({ ROOST_AGENT_CONVERSATION_RESTORE: "0" });
  });

  test.skipIf(process.platform === "win32")("forwards 0 and 1 through the POSIX binary worker installer", async () => {
    const platform = process.platform === "darwin" ? "darwin" : "linux";
    for (const value of ["0", "1"]) {
      const logs: string[] = [];
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl: "https://coord.example.test:4102",
        gitSha: GIT_SHA,
        cmd: "write-plist",
        env: { ROOST_AGENT_CONVERSATION_RESTORE: value },
        log: (message) => logs.push(message),
      });

      const definitionPath = logs.at(-1)?.split("→ ")[1];
      expect(definitionPath).toBeString();
      const definition = await Bun.file(definitionPath!).text();
      expect(parsePosixServiceEnvironment(definition, platform))
        .toMatchObject({ ROOST_AGENT_CONVERSATION_RESTORE: value });
      rmSync(dirname(definitionPath!), { recursive: true, force: true });
    }
  });

  test.skipIf(process.platform === "win32")("packaged POSIX install forwards ambient 0 and 1", async () => {
    const platform = process.platform === "darwin" ? "darwin" : "linux";
    for (const value of ["0", "1"]) {
      process.env.ROOST_AGENT_CONVERSATION_RESTORE = value;
      const logs: string[] = [];
      await installWorkerAgent({
        execPath: process.execPath,
        coordUrl: "https://coord.example.test:4102",
        gitSha: GIT_SHA,
        cmd: "write-plist",
        log: (message) => logs.push(message),
      });

      const definitionPath = logs.at(-1)?.split("→ ")[1];
      expect(definitionPath).toBeString();
      const definition = await Bun.file(definitionPath!).text();
      expect(parsePosixServiceEnvironment(definition, platform))
        .toMatchObject({ ROOST_AGENT_CONVERSATION_RESTORE: value });
      rmSync(dirname(definitionPath!), { recursive: true, force: true });
    }
  });
});
