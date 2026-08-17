import { describe, expect, test } from "bun:test";
import { resolve } from "node:path";
import { buildWindowsServiceDefinitions } from "../src/service-ctl.ts";
import { runWindowsUpdateBroker } from "../src/windows-update-broker.ts";
import { createServiceHealthProver } from "../src/windows-update-runtime.ts";
import type { WindowsUpdateJournalV1 } from "../src/windows-update-journal.ts";

describe("Windows service recovery topology", () => {
  test("persists install roots in every service and starts the updater at boot", () => {
    const serviceDir = "C:\\CustomRoost\\service";
    const versionsDir = "D:\\RoostVersions";
    const definitions = buildWindowsServiceDefinitions({
      executablePath: `${versionsDir}\\2.0.0\\roost.exe`,
      shawlPath: `${versionsDir}\\2.0.0\\shawl.exe`,
      serviceLauncherPath: "C:\\CustomRoost\\bin\\roost.exe",
      windowsHelperPath: `${versionsDir}\\2.0.0\\roost-win-helper.exe`,
      account: ".\\roost-operator",
      coordinatorHost: true,
      serviceDir,
      commonEnvironment: {
        ROOST_SERVICE_DIR: serviceDir,
        ROOST_VERSIONS_DIR: versionsDir,
        ROOST_WINDOWS_PUBLISHER_SHA256: "a".repeat(64),
      },
    });

    expect(definitions.updater.startMode).toBe("automatic");
    expect(definitions.updater.executablePath).toBe(`${versionsDir}\\2.0.0\\roost.exe`);
    expect(definitions.updater.arguments).toEqual(["__windows-updater-broker"]);
    const updaterDelimiter = definitions.updater.shawlArguments.lastIndexOf("--");
    expect(definitions.updater.shawlArguments.slice(updaterDelimiter + 1)).toEqual([
      `${versionsDir}\\2.0.0\\roost.exe`,
      "__windows-updater-broker",
    ]);
    for (const definition of Object.values(definitions)) {
      expect(definition.environment.ROOST_SERVICE_DIR).toBe(serviceDir);
      expect(definition.environment.ROOST_SERVICE_ROLE).toBe(definition.role);
      expect(definition.environment.ROOST_VERSIONS_DIR).toBe(versionsDir);
      expect(definition.shawlArguments).toContain(`ROOST_SERVICE_DIR=${serviceDir}`);
      expect(definition.shawlArguments).toContain(`ROOST_VERSIONS_DIR=${versionsDir}`);
      expect(definition.cwd.toLowerCase()).toStartWith(serviceDir.toLowerCase());
      expect(definition.logDir.toLowerCase()).toStartWith(serviceDir.toLowerCase());
    }
  });


  test("Windows join persists the requested worker label without persisting the token", () => {
    const root = resolve(import.meta.dir, "../../..");
    const script = `
      Object.defineProperty(process, "platform", { value: "win32" });
      const { installWorkerAgent } = await import("./apps/roost-cli/src/install-binary-agents.ts");
      const logs = [];
      await installWorkerAgent({
        execPath: "C:\\\\Roost\\\\versions\\\\2.0.0\\\\roost.exe",
        coordUrl: "https://coord.tail.example:4102",
        bootstrapToken: "one-shot-secret",
        gitSha: "2.0.0+abcdef12",
        cmd: "write-plist",
        env: {
          ROOST_SERVICE_ACCOUNT: ".\\\\roost-operator",
          ROOST_WINDOWS_PUBLISHER_SHA256: "${"a".repeat(64)}",
          ROOST_WORKER_LABEL: "Build PC",
        },
        log: (message) => logs.push(message),
      });
      const definitionPath = logs.at(-1).split("→ ")[1];
      process.stdout.write(await Bun.file(definitionPath).text());
    `;
    const result = Bun.spawnSync(["bun", "-e", script], {
      cwd: root,
      env: {
        ...process.env,
        LOCALAPPDATA: "C:\\Users\\tester\\AppData\\Local",
        USERPROFILE: "C:\\Users\\tester",
        ProgramData: "C:\\ProgramData",
        ROOST_INTERACTIVE_SID: "S-1-5-21-1-2-3-1001",
        ROOST_SYSTEM32: "C:\\Windows\\System32",
      },
    });
    expect(result.exitCode, result.stderr.toString()).toBe(0);
    const definitions = JSON.parse(result.stdout.toString()) as {
      worker: { environment: Record<string, string> };
    };
    expect(definitions.worker.environment.ROOST_WORKER_LABEL).toBe("Build PC");
    expect(definitions.worker.environment.ROOST_BOOTSTRAP_TOKEN).toBeUndefined();
    const serviceHome = definitions.worker.environment.USERPROFILE.replaceAll("\\", "/");
    expect(serviceHome).toBe("C:/ProgramData/Roost/service/home");
    expect(definitions.worker.environment.HOME.replaceAll("\\", "/")).toBe(serviceHome);
    expect(definitions.worker.environment.APPDATA.replaceAll("\\", "/")).toBe(`${serviceHome}/AppData/Roaming`);
    expect(definitions.worker.environment.LOCALAPPDATA.replaceAll("\\", "/")).toBe(`${serviceHome}/AppData/Local`);
    expect(definitions.worker.environment.TEMP.replaceAll("\\", "/")).toBe(`${serviceHome}/AppData/Local/Temp`);
    expect(definitions.worker.environment.TMP.replaceAll("\\", "/")).toBe(`${serviceHome}/AppData/Local/Temp`);
  });

  test("boot-time updater exits cleanly when no transaction needs recovery", async () => {
    let contextChecks = 0;
    let releases = 0;
    const result = await runWindowsUpdateBroker({
      store: {
        path: "C:\\Roost\\service\\windows-update-journal.json",
        load: async () => null,
        save: async () => { throw new Error("idle updater must not save"); },
      },
      services: {} as never,
      native: {
        assertUpdaterServiceContext: async () => { contextChecks += 1; },
      } as never,
      health: {} as never,
      acquireTransaction: async (_kind, journalPath) => ({
        schemaVersion: 1,
        kind: "update",
        journalPath,
        ownerPid: process.pid,
        processEpoch: "test-epoch",
        acquiredAt: "2026-08-16T00:00:00.000Z",
        lockPath: "C:\\Roost\\service\\machine-transaction.lock",
        release: async () => { releases += 1; },
      }),
    });

    expect(result).toBeNull();
    expect(contextChecks).toBe(1);
    expect(releases).toBe(1);
  });

  test("forward health requires the exact signed build identity", async () => {
    const journal = {
      targetVersion: "2.0.0",
      targetBuild: "a".repeat(40),
      healthBefore: {
        worker: {
          version: "1.0.0",
          build: "b".repeat(40),
          processEpoch: "prior",
          coordinatorUrl: "https://coord.example.test",
        },
      },
      stoppedRoles: ["worker"],
    } as unknown as WindowsUpdateJournalV1;
    const descriptor = {
      role: "worker" as const,
      version: "2.0.0",
      build: "c".repeat(40),
      processEpoch: "next",
      targetLinkReady: true,
      coordinatorUrl: "https://coord.example.test",
    };
    let clock = 0;
    const mismatch = createServiceHealthProver(
      { read: async () => descriptor },
      { timeoutMs: 1, now: () => clock++, sleep: async () => undefined },
    );
    await expect(mismatch.prove("worker", journal, "forward"))
      .rejects.toThrow("exact expected version/build/process epoch");

    const exact = createServiceHealthProver({
      read: async () => ({ ...descriptor, build: journal.targetBuild }),
    });
    await exact.prove("worker", journal, "forward");
  });
});
