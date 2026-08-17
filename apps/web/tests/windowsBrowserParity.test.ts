import { describe, expect, test } from "bun:test";
import {
  machineActionHref,
  machineActionsForWorker,
  machinePlatformIcon,
  remoteDesktopFile,
  windowsSharePath,
} from "../src/lib/machineActions.ts";
import { buildMachineJoinCommand } from "@roost/shared/machine-join-command";

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
    const command = buildMachineJoinCommand("win32", "https://coord.tail.example", "tok'en", "Build PC", "a".repeat(64));
    expect(command).toContain("releases/download/$($r.tag_name)");
    expect(command).toContain("Get-AuthenticodeSignature");
    expect(command).toContain("-CoordinatorUrl 'https://coord.tail.example'");
    expect(command).toContain("$t=ConvertTo-SecureString 'tok''en' -AsPlainText -Force");
    expect(command).toContain("-BootstrapToken $t");
    expect(command).toContain("-WorkerLabel 'Build PC'");
    expect(command).toContain("-ReleaseBaseUrl $b");
    expect(command).toContain("$h='" + "a".repeat(64) + "'");
    expect(command).toContain("$s.SignerCertificate.RawData");
    expect(command).toContain("$null -eq $s.TimeStamperCertificate");
    expect(command).toContain("if($a -cne $h)");
    expect(command).toContain("-PublisherSha256 $h");
    expect(command).toContain("Set-ExecutionPolicy -Scope Process -ExecutionPolicy RemoteSigned -Force");
    expect(command).not.toContain("iex");
  });

  test("Windows join command rejects a missing or malformed independent publisher pin", () => {
    expect(() => buildMachineJoinCommand("win32", "https://coord", "token", "", "")).toThrow(
      "trusted 64-hex release-publisher SHA-256",
    );
    expect(() => buildMachineJoinCommand("win32", "https://coord", "token", "", "g".repeat(64))).toThrow(
      "trusted 64-hex release-publisher SHA-256",
    );
  });

  test("POSIX join command remains byte-compatible", () => {
    expect(buildMachineJoinCommand("darwin", "https://coord", "token", "mac mini")).toBe(
      "curl -fsSL https://raw.githubusercontent.com/cefege/roost/main/join.sh | "
      + "ROOST_COORDINATOR_URL=\"https://coord\" ROOST_BOOTSTRAP_TOKEN=\"token\""
      + " ROOST_WORKER_LABEL=\"mac mini\" bash",
    );
  });
});
