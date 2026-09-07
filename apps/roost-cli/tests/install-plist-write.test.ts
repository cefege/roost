// Guards the macOS LaunchAgent write shared by both POSIX installers. launchd
// silently declines anything that is not a complete plist and reports nothing
// for a job it never loaded, so a half-written agent reads as a machine that
// simply went quiet. These tests drive apps/{worker,coord}/scripts/install.sh
// through `write-plist` and pin that a rejected write keeps the installed agent.

import { describe, expect, test } from "bun:test";
import fs from "node:fs";
import { join } from "node:path";
import { parsePosixServiceEnvironment } from "../src/deploy-plist-env.ts";

type ServiceRole = "worker" | "coord";

const SENTINEL_PLIST = [
  "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
  "<plist version=\"1.0\">",
  "<dict>",
  "  <key>Label</key>",
  "  <string>previously-installed</string>",
  "</dict>",
  "</plist>",
  "",
].join("\n");

interface FakeMacHost {
  readonly installer: string;
  readonly plist: string;
  readonly agentsDir: string;
  readonly env: Record<string, string>;
}

/** Lay out a throwaway checkout whose installer writes into a fake $HOME.
 *  `uname` is stubbed to Darwin so the launchd branch runs on any CI host. */
function fakeMacHost(role: ServiceRole, stubs: Record<string, string> = {}): FakeMacHost {
  const root = fs.mkdtempSync(join(process.env.TMPDIR ?? "/tmp", `plist-${role}-`));
  const scriptDir = join(root, "repo", `apps/${role === "worker" ? "worker" : "coord"}/scripts`);
  const installer = join(scriptDir, "install.sh");
  const home = join(root, "home");
  const agentsDir = join(home, "Library/LaunchAgents");
  const bin = join(root, "bin");
  fs.mkdirSync(scriptDir, { recursive: true });
  fs.mkdirSync(agentsDir, { recursive: true });
  fs.mkdirSync(bin, { recursive: true });
  fs.copyFileSync(
    join(import.meta.dir, `../../${role === "worker" ? "worker" : "coord"}/scripts/install.sh`),
    installer,
  );
  fs.chmodSync(installer, 0o700);
  for (const [name, body] of Object.entries({ uname: "#!/usr/bin/env bash\necho Darwin\n", ...stubs })) {
    fs.writeFileSync(join(bin, name), body);
    fs.chmodSync(join(bin, name), 0o700);
  }
  const plist = join(agentsDir, `test-${role}.plist`);
  const shared: Record<string, string> = {
    HOME: home,
    PATH: `${bin}:/usr/bin:/bin`,
    BUN_BIN: "/bin/true",
  };
  const env = role === "worker"
    ? {
      ...shared,
      ROOST_WORKER_PLIST: plist,
      ROOST_WORKER_AGENT_LABEL: `test-${role}`,
      ROOST_COORDINATOR_URL: "https://coord.example:4102/?a=1&b=<worker>",
      ROOST_WORKER_DATA_DIR: join(root, "data"),
      ROOST_WORKER_LOG_DIR: join(root, "logs"),
    }
    : {
      ...shared,
      ROOST_COORD_PLIST: plist,
      ROOST_COORD_LABEL: `test-${role}`,
      ROOST_COORD_DATA_DIR: join(root, "data"),
      ROOST_COORD_LOG_DIR: join(root, "logs"),
    };
  return { installer, plist, agentsDir, env };
}

function writePlist(host: FakeMacHost): { exitCode: number; stderr: string } {
  const proc = Bun.spawnSync(["bash", host.installer, "write-plist"], { env: host.env });
  return { exitCode: proc.exitCode ?? 1, stderr: new TextDecoder().decode(proc.stderr) };
}

/** Staging files live beside the target; a rejected write must clean up. */
function residue(host: FakeMacHost): string[] {
  return fs.readdirSync(host.agentsDir).filter((name) => !name.endsWith(".plist"));
}

describe("macOS LaunchAgent write", () => {
  for (const role of ["worker", "coord"] as const) {
    test(`${role} installer publishes a complete plist launchd can load`, () => {
      const host = fakeMacHost(role);

      expect(writePlist(host).exitCode).toBe(0);

      const document = fs.readFileSync(host.plist, "utf8");
      expect(document).toContain("<key>ProgramArguments</key>");
      expect(document).toContain("<key>EnvironmentVariables</key>");
      expect(document).toContain("<key>KeepAlive</key>");
      expect(document.trimEnd().endsWith("</plist>")).toBe(true);
      expect(fs.statSync(host.plist).mode & 0o777).toBe(0o600);
      expect(residue(host)).toEqual([]);
      // The reuse path on the next deploy reads this file back; it must round-trip.
      expect(parsePosixServiceEnvironment(document, "darwin")).toMatchObject(
        role === "worker"
          ? { ROOST_COORDINATOR_URL: "https://coord.example:4102/?a=1&b=<worker>" }
          : { ROOST_COORDINATOR_BIND: expect.any(String) },
      );
    });

    test(`${role} installer keeps the installed agent when the write dies mid-document`, () => {
      // A truncated body is the shape an interrupted or short write leaves behind.
      const host = fakeMacHost(role, { cat: "#!/usr/bin/env bash\nhead -c 120\nexit 1\n" });
      fs.writeFileSync(host.plist, SENTINEL_PLIST, { mode: 0o600 });

      expect(writePlist(host).exitCode).not.toBe(0);

      expect(fs.readFileSync(host.plist, "utf8")).toBe(SENTINEL_PLIST);
      expect(residue(host)).toEqual([]);
    });

    test(`${role} installer keeps the installed agent when the document fails plist validation`, () => {
      const host = fakeMacHost(role, { plutil: "#!/usr/bin/env bash\nexit 1\n" });
      fs.writeFileSync(host.plist, SENTINEL_PLIST, { mode: 0o600 });

      const result = writePlist(host);

      expect(result.exitCode).not.toBe(0);
      expect(result.stderr).toContain("refusing to install a malformed");
      expect(fs.readFileSync(host.plist, "utf8")).toBe(SENTINEL_PLIST);
      expect(residue(host)).toEqual([]);
    });
  }
});
