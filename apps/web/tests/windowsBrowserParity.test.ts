import { describe, expect, test } from "bun:test";
import {
  machineActionHref,
  machineActionsForWorker,
  machinePlatformIcon,
  remoteDesktopFile,
  windowsSharePath,
} from "../src/lib/machineActions.ts";
import { buildMachineJoinCommand } from "../src/lib/machineJoinCommand.ts";

describe("Windows browser machine surfaces", () => {
  test("uses Windows icon and exposes only native RDP/share actions", () => {
    expect(machinePlatformIcon("win32")).toBe("desktop_windows");
    expect(machineActionsForWorker("win32", true).map((action) => action.id))
      .toEqual(["network-share", "remote-desktop"]);
    expect(machineActionsForWorker("linux", true)).toEqual([]);
    expect(machineActionsForWorker("darwin", true).map((action) => action.id))
      .toEqual(["finder", "screen-share"]);
  });

  test("keeps existing macOS protocol URLs and emits native Windows payloads", () => {
    expect(machineActionHref("finder", "mac.tail.example")).toBe("smb://mac.tail.example");
    expect(machineActionHref("screen-share", "mac.tail.example")).toBe("vnc://mac.tail.example");
    expect(windowsSharePath("win.tail.example")).toBe("\\\\win.tail.example\\");
    expect(remoteDesktopFile("win.tail.example")).toContain("full address:s:win.tail.example\r\n");
  });

  test("Windows join command downloads and verifies the signed release script", () => {
    const command = buildMachineJoinCommand("win32", "https://coord.tail.example", "tok'en", "Build PC");
    expect(command).toContain("releases/latest/download/join.ps1");
    expect(command).toContain("Get-AuthenticodeSignature");
    expect(command).toContain("-CoordinatorUrl 'https://coord.tail.example'");
    expect(command).toContain("-BootstrapToken 'tok''en'");
    expect(command).toContain("-WorkerLabel 'Build PC'");
    expect(command).not.toContain("ExecutionPolicy");
    expect(command).not.toContain("iex");
  });

  test("POSIX join command remains byte-compatible", () => {
    expect(buildMachineJoinCommand("darwin", "https://coord", "token", "mac mini")).toBe(
      "curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | "
      + "ROOST_COORDINATOR_URL=\"https://coord\" ROOST_BOOTSTRAP_TOKEN=\"token\""
      + " ROOST_WORKER_LABEL=\"mac mini\" bash",
    );
  });
});
