// Worker service runtime parser tests pin installer-emitted POSIX command shapes.
// Source mode preserves an absolute Bun executable; binary mode never masquerades as Bun.
// Malformed service text surfaces runtime_unavailable metadata before mutation.

import { describe, expect, test } from "bun:test";
import { DeployFailure } from "../src/deploy-exec.ts";
import {
  parseWorkerServiceRuntime,
  requireSourceWorkerServiceRuntime,
  resolveRemoteWorkerRuntime,
} from "../src/worker-service-runtime.ts";

function expectRuntimeUnavailable(action: () => void): void {
  try {
    action();
  } catch (error) {
    expect(error).toBeInstanceOf(DeployFailure);
    expect(error).toMatchObject({
      workerUpdateFailure: {
        code: "runtime_unavailable",
        phase: "preflight",
      },
    });
    return;
  }
  throw new Error("expected worker runtime refusal");
}

describe("parseWorkerServiceRuntime", () => {
  test("parses installer-quoted Linux source execution", () => {
    const definition = [
      "[Service]",
      'ExecStart="/opt/homebrew/bin/bun" "/srv/Roost Worker/apps/worker/src/main.ts"',
    ].join("\n");
    expect(parseWorkerServiceRuntime(definition, "linux")).toEqual({
      executable: "/opt/homebrew/bin/bun",
      mode: "source",
    });
  });

  test("parses LaunchAgent source and binary execution separately", () => {
    const sourceDefinition = [
      "<plist><dict>",
      "<key>ProgramArguments</key><array>",
      "<string>/Users/roost/.bun/bin/bun</string>",
      "<string>/Users/roost/Roost/apps/worker/src/main.ts</string>",
      "</array></dict></plist>",
    ].join("\n");
    const binaryDefinition = sourceDefinition.replace(
      "/Users/roost/.bun/bin/bun</string>\n<string>/Users/roost/Roost/apps/worker/src/main.ts",
      "/Applications/Roost/roost</string>\n<string>worker",
    );
    expect(parseWorkerServiceRuntime(sourceDefinition, "darwin")).toEqual({
      executable: "/Users/roost/.bun/bin/bun",
      mode: "source",
    });
    expect(parseWorkerServiceRuntime(binaryDefinition, "darwin")).toEqual({
      executable: "/Applications/Roost/roost",
      mode: "binary",
    });
  });

  test("refuses shell-shaped and binary runtimes before source mutation", () => {
    const shellDefinition = 'ExecStart=bun /srv/Roost/apps/worker/src/main.ts';
    expect(parseWorkerServiceRuntime(shellDefinition, "linux")).toBeNull();
    expectRuntimeUnavailable(() => requireSourceWorkerServiceRuntime(shellDefinition, "linux"));
    const binaryDefinition = 'ExecStart="/Applications/Roost/roost" "worker"';
    expectRuntimeUnavailable(() => requireSourceWorkerServiceRuntime(binaryDefinition, "linux"));
  });

  test("probes the installed executable without PATH fallback", async () => {
    const commands: string[] = [];
    const runtime = await resolveRemoteWorkerRuntime(
      'ExecStart="/opt/pinned/bun" "/srv/roost/apps/worker/src/main.ts"',
      "linux",
      async (command) => {
        commands.push(command);
        return {
          exit: 0,
          stdout: JSON.stringify({
            bunAbi: "1.3.14",
            platform: "linux",
            arch: "x64",
          }),
          stderr: "",
        };
      },
    );
    expect(runtime.executable).toBe("/opt/pinned/bun");
    expect(commands).toHaveLength(1);
    expect(commands[0]).toContain("'/opt/pinned/bun'");
    expect(commands[0]).not.toContain("command -v bun");
  });

  test("discovers Bun only when no service is installed", async () => {
    const commands: string[] = [];
    const runtime = await resolveRemoteWorkerRuntime(null, "darwin", async (command) => {
      commands.push(command);
      if (command === "command -v bun") {
        return { exit: 0, stdout: "/Users/roost/.bun/bin/bun\n", stderr: "" };
      }
      return {
        exit: 0,
        stdout: JSON.stringify({
          bunAbi: "1.3.14",
          platform: "darwin",
          arch: "arm64",
        }),
        stderr: "",
      };
    });
    expect(runtime.executable).toBe("/Users/roost/.bun/bin/bun");
    expect(commands[0]).toBe("command -v bun");
    expect(commands[1]).toContain("'/Users/roost/.bun/bin/bun'");
  });
});
