import { afterEach, describe, expect, test } from "bun:test";
import { chmodSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { parse as parseYaml } from "yaml";
import { expose, type CommandResult, type ExposeDependencies } from "../src/expose.ts";

const HOST = "roost.example.com";
const TEAM = "example.cloudflareaccess.com";
const AUD = "a".repeat(64);
const cleanups: Array<() => void> = [];

afterEach(() => {
  process.exitCode = 0;
  for (const cleanup of cleanups.splice(0)) cleanup();
});

function fixture(yaml = `ingress:\n  - hostname: ${HOST}\n    service: http://127.0.0.1:4104\n  - service: http_status:404\n`): {
  dir: string;
  config: string;
} {
  const dir = mkdtempSync(join(tmpdir(), "roost-expose-test-"));
  const config = join(dir, "config.yml");
  writeFileSync(config, yaml);
  cleanups.push(() => rmSync(dir, { recursive: true, force: true }));
  return { dir, config };
}

function dependencies(
  config: string,
  options: {
    result?: (command: string[]) => CommandResult;
    sourceInvocation?: boolean;
    repoRoot?: string;
    installSource?: (env: Record<string, string>) => Promise<void>;
    installBinary?: (env: Record<string, string>) => Promise<void>;
  } = {},
): { deps: Partial<ExposeDependencies>; commands: string[][]; logs: string[] } {
  const commands: string[][] = [];
  const logs: string[] = [];
  const result = options.result ?? ((command: string[]) => ({
    exit: 0,
    stdout: command.at(-2) === "rule" ? "service: http://127.0.0.1:4104\n" : "",
    stderr: "",
  }));
  return {
    commands,
    logs,
    deps: {
      which: () => "/usr/bin/cloudflared",
      run: (command) => {
        commands.push(command);
        return result(command);
      },
      parseYaml,
      sourceInvocation: options.sourceInvocation ?? false,
      repoRoot: options.repoRoot ?? fixture().dir,
      home: "/unused",
      installSource: options.installSource ?? (async () => {}),
      installBinary: options.installBinary ?? (async () => {}),
      log: (message) => logs.push(message),
      error: (message) => logs.push(message),
    },
  };
}

function args(config: string): string[] {
  return [HOST, "--team", TEAM, "--aud", AUD, "--config", config];
}

describe("roost expose", () => {
  test("validates the exact first rule and all dangerous paths before install", async () => {
    const { config } = fixture();
    let installed: Record<string, string> | undefined;
    const { deps, commands, logs } = dependencies(config, {
      installBinary: async (env) => { installed = env; },
    });
    await expose(args(config), deps);
    expect(commands).toHaveLength(5);
    expect(commands[0]).toEqual(["cloudflared", "--config", config, "tunnel", "ingress", "validate"]);
    expect(commands.slice(1).map((command) => command.at(-1))).toEqual([
      `https://${HOST}/`,
      `https://${HOST}/api/db-export`,
      `https://${HOST}/internal/coord-handoff/commit`,
      `https://${HOST}/ws/coord-worker/${"a".repeat(64)}`,
    ]);
    expect(installed).toEqual({
      ROOST_FRONTED: "1",
      ROOST_PUBLIC_BIND: "127.0.0.1:4104",
      ROOST_CF_ACCESS_TEAM_DOMAIN: TEAM,
      ROOST_CF_ACCESS_AUD: AUD,
      ROOST_WEB_PUBLIC_URL: `https://${HOST}`,
    });
    expect(logs).toContain(`sudo cloudflared --config ${config} service install`);
  });

  test("rejects deceptive, path-bearing, wildcard, and private first rules", async () => {
    const invalid = [
      `ingress:\n  - hostname: ${HOST}\n    path: /safe\n    service: http://127.0.0.1:4104\n`,
      `ingress:\n  - hostname: "*.example.com"\n    service: http://127.0.0.1:4104\n  - hostname: ${HOST}\n    service: http://127.0.0.1:4104\n`,
      `ingress:\n  - hostname: ${HOST}\n    service: http://127.0.0.1:4103\n  - service: "service: http://127.0.0.1:4104 hostname: ${HOST}"\n`,
    ];
    for (const yaml of invalid) {
      const { config } = fixture(yaml);
      let installed = false;
      const { deps } = dependencies(config, { installBinary: async () => { installed = true; } });
      await expect(expose(args(config), deps)).rejects.toThrow("first ingress rule");
      expect(installed).toBe(false);
    }
  });

  test("rejects invalid YAML, missing ingress, and non-object aliases", async () => {
    for (const [yaml, message] of [
      ["ingress: [\n", "invalid cloudflared YAML"],
      ["tunnel: unrelated\n", "first ingress rule"],
      ["bad: &bad scalar\ningress:\n  - *bad\n", "first ingress rule"],
    ] as const) {
      const { config } = fixture(yaml);
      const { deps } = dependencies(config);
      await expect(expose(args(config), deps)).rejects.toThrow(message);
    }
  });

  test("accepts current cloudflared rule diagnostics around the matched service", async () => {
    const { config } = fixture();
    const { deps } = dependencies(config, {
      result: (command) => ({
        exit: 0,
        stdout: command.at(-2) === "rule"
          ? `Using rules from ${config}\nMatched rule #0\n\thostname: ${HOST}\n\tservice: http://127.0.0.1:4104\n`
          : "",
        stderr: "",
      }),
    });
    await expect(expose(args(config), deps)).resolves.toBeUndefined();
  });

  test("rejects every probe mismatch and a nonzero cloudflared command", async () => {
    const { config } = fixture();
    for (const failingCall of [0, 1, 2, 3, 4]) {
      let call = 0;
      let installed = false;
      const { deps } = dependencies(config, {
        result: (command) => {
          const current = call++;
          if (current === failingCall) return { exit: current === 0 ? 1 : 0, stdout: "service: http://127.0.0.1:4103\n", stderr: "bad" };
          return { exit: 0, stdout: command.at(-2) === "rule" ? "service: http://127.0.0.1:4104\n" : "", stderr: "" };
        },
        installBinary: async () => { installed = true; },
      });
      await expect(expose(args(config), deps)).rejects.toThrow();
      expect(installed).toBe(false);
    }
  });

  test("source install rollback preserves exact env bytes and mode", async () => {
    const { dir, config } = fixture();
    const envPath = join(dir, ".env.local");
    const before = Buffer.from("UNRELATED='exact bytes'\nROOST_PUBLIC_BIND=old\n");
    writeFileSync(envPath, before);
    chmodSync(envPath, 0o640);
    const { deps } = dependencies(config, {
      sourceInvocation: true,
      repoRoot: dir,
      installSource: async () => { throw new Error("readiness failed"); },
    });
    await expect(expose(args(config), deps)).rejects.toThrow("readiness failed");
    expect(readFileSync(envPath)).toEqual(before);
    expect(statSync(envPath).mode & 0o777).toBe(0o640);
  });

  test("invalid arguments exit 2 and print prerequisites in order", async () => {
    const logs: string[] = [];
    const { deps } = dependencies("/missing");
    await expose(["https://bad.example.com"], {
      ...deps,
      log: (message) => logs.push(message),
      error: (message) => logs.push(message),
    });
    expect(process.exitCode).toBe(2);
    expect(logs.join("\n")).toContain("cloudflared tunnel login\ncloudflared tunnel create roost\ncloudflared tunnel route dns roost <hostname>");
  });
});

  test("installer persists public settings in Linux and macOS service definitions", () => {
    for (const platform of ["Linux", "Darwin"]) {
      const { dir } = fixture();
      const bin = join(dir, "bin");
      mkdirSync(bin);
      const uname = join(bin, "uname");
      writeFileSync(uname, `#!/bin/sh\nprintf '%s\\n' '${platform}'\n`);
      chmodSync(uname, 0o755);
      const definition = join(dir, platform === "Linux" ? "coord.service" : "coord.plist");
      const result = Bun.spawnSync(["bash", "apps/coord/scripts/install.sh", "write-plist"], {
        cwd: resolve(import.meta.dir, "../../.."),
        env: {
          ...process.env,
          PATH: `${bin}:/usr/bin:/bin`,
          HOME: dir,
          BUN_BIN: "/usr/bin/true",
          ROOST_REPO_ROOT: resolve(import.meta.dir, "../../.."),
          ROOST_SKIP_ENV_LOCAL: "1",
          ROOST_COORD_UNIT: definition,
          ROOST_COORD_PLIST: definition,
          ROOST_COORD_DATA_DIR: join(dir, "data"),
          ROOST_COORD_LOG_DIR: join(dir, "logs"),
          ROOST_FRONTED: "1",
          ROOST_PUBLIC_BIND: "127.0.0.1:4104",
          ROOST_WEB_PUBLIC_URL: `https://${HOST}`,
          ROOST_CF_ACCESS_TEAM_DOMAIN: TEAM,
          ROOST_CF_ACCESS_AUD: AUD,
        },
      });
      expect(result.exitCode, result.stderr.toString()).toBe(0);
      const service = readFileSync(definition, "utf8");
      for (const value of ["127.0.0.1:4104", `https://${HOST}`, TEAM, AUD]) {
        expect(service).toContain(value);
      }
    }
  });
