// systemd unit quoting is NOT universal, and both installers used to assume it was.
//
// `apps/coord/scripts/install.sh` and `apps/worker/scripts/install.sh` piped every
// dynamic value through systemd_quote(), which wraps it in double quotes. systemd
// honours quoting only in command lines (ExecStart=) and Environment=. For the
// path/specifier settings it is wrong in two different ways:
//   WorkingDirectory="/repo"                 -> "path is not absolute" -> the unit
//                                               has a fatal setting and REFUSES to start
//   StandardOutput="append:/log/main.out.log" -> "Failed to parse output specifier",
//                                               silently ignored -> service logs vanish
// Production impact: `roost push` staged a coordinator release, wrote the new unit,
// systemd refused to start it, push rolled the release back — the whole fleet stuck
// at an old commit, every Linux coordinator/worker deploy failing identically.
//
// The fix added systemd_path() beside systemd_quote(): it rejects control characters
// and `"`, doubles `%` so the value is never read as a systemd specifier, and prints
// the value RAW. This file drives the real installers through their `write-plist`
// verb — the verb `roost push` uses to stage a unit — and pins both halves of the
// rule: paths raw, ExecStart quoted.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import fs from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const REPO_ROOT = resolve(import.meta.dir, "../../..");
// systemd units are a Linux-only artifact: on macOS `write-plist` takes the launchd
// branch, which is XML and carries no quoting rule to regress.
const NOT_LINUX = process.platform !== "linux";
// `systemd-analyze --user verify` is the assertion that actually reproduces the
// production failure — it is systemd's own parser saying no. The string assertions
// below are the portable half, kept meaningful on a host without systemd installed.
const SYSTEMD_ANALYZE = Bun.which("systemd-analyze");
// WorkingDirectory=, StandardOutput=, StandardError= — the three settings that take
// a raw value. Every one of them was quoted before the fix.
const PATH_DIRECTIVES = ["WorkingDirectory", "StandardOutput", "StandardError"] as const;

let root = "";

beforeEach(() => {
  root = fs.mkdtempSync(join(tmpdir(), "roost-unit-quoting-"));
});

afterEach(() => {
  fs.rmSync(root, { recursive: true, force: true });
});

/** Generate one service's systemd unit through the installer's `write-plist` verb.
 *
 *  The log directory name carries a literal `%`, so the unit must come back with it
 *  doubled: an un-doubled `%d`/`%h` in a path setting is expanded by systemd into
 *  something else entirely (or fails to parse), which is the second half of the bug
 *  systemd_path() closes.
 *
 *  The environment is built from scratch rather than inherited: the installers read a
 *  long list of ROOST_* variables, and a developer's own exports must not reach the
 *  generated unit. */
function writeUnit(service: "coord" | "worker"): { unit: string; text: string } {
  const home = join(root, `${service}-home`);
  const logDir = join(root, `${service}-log%state`);
  const unit = join(root, `${service}.service`);
  fs.mkdirSync(home, { recursive: true });
  const env: Record<string, string> = {
    PATH: "/usr/bin:/bin",
    HOME: home,
    // ExecStart's target is never executed here; it only has to be a resolvable path
    // so the generated unit is the one a real from-source install would write.
    BUN_BIN: "/bin/true",
    ROOST_SKIP_ENV_LOCAL: "1",
  };
  let script: string;
  if (service === "coord") {
    script = join(REPO_ROOT, "apps/coord/scripts/install.sh");
    Object.assign(env, {
      ROOST_REPO_ROOT: REPO_ROOT,
      ROOST_COORD_UNIT: unit,
      ROOST_COORD_DATA_DIR: join(root, "coord-data"),
      ROOST_COORD_LOG_DIR: logDir,
    });
  } else {
    // The worker installer derives REPO_ROOT from its own location and has no
    // ROOST_SKIP_ENV_LOCAL seam, so it is copied into a throwaway repo — the same
    // trick deploy-plist-env.test.ts uses — to keep the repo's .env.local out of
    // the unit. It is copied from the shipping script on every run, so this still
    // tests the real installer.
    const scriptDir = join(root, "worker-repo/apps/worker/scripts");
    fs.mkdirSync(scriptDir, { recursive: true });
    script = join(scriptDir, "install.sh");
    fs.copyFileSync(join(REPO_ROOT, "apps/worker/scripts/install.sh"), script);
    Object.assign(env, {
      ROOST_COORDINATOR_URL: "https://coord.test:4102",
      ROOST_WORKER_UNIT: unit,
      ROOST_WORKER_DATA_DIR: join(root, "worker-data"),
      ROOST_WORKER_LOG_DIR: logDir,
    });
  }
  const proc = Bun.spawnSync(["bash", script, "write-plist"], { env });
  expect(proc.exitCode, proc.stderr.toString()).toBe(0);
  return { unit, text: fs.readFileSync(unit, "utf8") };
}

/** The value of `key=` in the unit, i.e. exactly the bytes systemd parses. */
function directive(text: string, key: string): string {
  const line = text.split("\n").find((l) => l.startsWith(`${key}=`));
  if (line === undefined) throw new Error(`generated unit has no ${key}= line`);
  return line.slice(key.length + 1);
}

describe.skipIf(NOT_LINUX)("systemd unit quoting", () => {
  for (const service of ["coord", "worker"] as const) {
    test(`${service}: path settings are raw and %-escaped, ExecStart stays quoted`, () => {
      const { text } = writeUnit(service);

      for (const key of PATH_DIRECTIVES) {
        const value = directive(text, key);
        // The bug: `WorkingDirectory="/repo"`. A quoted path is fatal (WorkingDirectory)
        // or silently dropped (StandardOutput/StandardError).
        expect(value, key).not.toMatch(/^"/);
        expect(value, key).not.toMatch(/"$/);
        // Every `%` must be doubled; a lone one is a specifier systemd would expand.
        expect(value.replaceAll("%%", ""), key).not.toContain("%");
      }
      expect(directive(text, "WorkingDirectory")).toStartWith("/");
      // The log dir's literal `%` survived, doubled — proof the escape ran at all
      // rather than the assertion above passing on a `%`-free path.
      expect(directive(text, "StandardOutput")).toContain(`${service}-log%%state`);
      expect(directive(text, "StandardError")).toContain(`${service}-log%%state`);

      // The other direction must not regress: ExecStart IS a command line, where
      // quoting is the supported way to carry a path containing spaces.
      expect(directive(text, "ExecStart")).toStartWith('"');
    });

    test.skipIf(SYSTEMD_ANALYZE === null)(`${service}: systemd's own parser accepts the unit`, () => {
      const { unit } = writeUnit(service);
      const proc = Bun.spawnSync([SYSTEMD_ANALYZE as string, "--user", "verify", unit], {
        env: {
          PATH: "/usr/bin:/bin",
          // --user needs a runtime dir to build its manager; without one verify fails
          // for an unrelated reason and would pass this test vacuously.
          XDG_RUNTIME_DIR: process.env.XDG_RUNTIME_DIR ?? `/run/user/${process.getuid()}`,
        },
      });
      const output = `${proc.stdout.toString()}${proc.stderr.toString()}`;
      expect(output).not.toContain("Failed to initialize manager");
      // Pre-fix, verify prints exactly these: "path is not absolute" escalates to a
      // fatal setting, and the quoted output specifier is discarded.
      expect(output).not.toContain("bad unit file setting");
      expect(output).not.toContain("Failed to parse output specifier");
    });
  }
});
