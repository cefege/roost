// _backfillEnvFromPlist purity + parsing contract.
//
// Regression guard for the env-backfill leak: the function used to write the
// values it parsed straight into process.env. `roost push` deploys every
// target serially in ONE process, so the first host to supply
// ROOST_WORKER_LABEL / ROOST_REACHABLE_ADDR poisoned every later target —
// ovh1-8c32g got installed with mike-m5-air's label and reachable address,
// and reachable_addr is what the coordinator-move preflight builds target_url
// from. The load-bearing assertion here is that process.env is untouched.
//
// Only the `host === "self"` branch is exercised: the remote branch shells out
// through sshExec → a real `ssh` process, which a unit test must not do.
import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import fs from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { _backfillEnvFromPlist, _resolveDeployEnvValue } from "../src/deploy-plist-env.ts";
import { WORKER_UNIT } from "../src/service-ctl.ts";

const KEYS = ["ROOST_COORDINATOR_URL", "ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL", "HOME"] as const;

let root = "";
let saved: Record<string, string | undefined> = {};

/** A fake $HOME for one machine. `plist` / `unit` are written verbatim so a
 *  test can assert on the exact on-disk syntax the parsers key off. */
function fakeHost(name: string, files: { plist?: string; unit?: string }): string {
  const home = join(root, name);
  if (files.plist !== undefined) {
    fs.mkdirSync(join(home, "Library/LaunchAgents"), { recursive: true });
    fs.writeFileSync(join(home, "Library/LaunchAgents/com.roost.worker-v2.plist"), files.plist);
  }
  if (files.unit !== undefined) {
    fs.mkdirSync(join(home, ".config/systemd/user"), { recursive: true });
    fs.writeFileSync(join(home, ".config/systemd/user", WORKER_UNIT), files.unit);
  }
  fs.mkdirSync(home, { recursive: true });
  return home;
}

function plistWith(entries: Record<string, string>): string {
  const body = Object.entries(entries)
    .map(([k, v]) => `\t\t<key>${k}</key>\n\t\t<string>${v}</string>`)
    .join("\n");
  return `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
\t<key>EnvironmentVariables</key>
\t<dict>
${body}
\t</dict>
</dict>
</plist>
`;
}

function unitWith(entries: Record<string, string>): string {
  const body = Object.entries(entries).map(([k, v]) => `Environment=${k}=${v}`).join("\n");
  return `[Unit]\nDescription=roost worker\n\n[Service]\n${body}\nExecStart=/usr/bin/true\n`;
}

/** Point the "self" branch at `home` and read the host's record. */
function backfillFrom(home: string): Promise<{ env: Record<string, string>; filled: string[] }> {
  process.env.HOME = home;
  return _backfillEnvFromPlist("self");
}

beforeEach(() => {
  saved = {};
  for (const k of KEYS) saved[k] = process.env[k];
  // `missing` is computed off ambient process.env: a stray ROOST_* in the
  // dev shell or a sibling test file would short-circuit the whole function
  // and make these tests pass for the wrong reason.
  delete process.env.ROOST_COORDINATOR_URL;
  delete process.env.ROOST_REACHABLE_ADDR;
  delete process.env.ROOST_WORKER_LABEL;
  root = fs.mkdtempSync(join(tmpdir(), "roost-plist-env-"));
});

afterEach(() => {
  for (const k of KEYS) {
    if (saved[k] === undefined) delete process.env[k];
    else process.env[k] = saved[k];
  }
  fs.rmSync(root, { recursive: true, force: true });
});

describe("_backfillEnvFromPlist — purity across serial deploys", () => {
  test("does not write what it parsed into process.env", async () => {
    const home = fakeHost("m5-air", {
      plist: plistWith({
        ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
        ROOST_REACHABLE_ADDR: "mike-m5-air.tail1234.ts.net",
        ROOST_WORKER_LABEL: "mike-m5-air",
      }),
    });

    const r = await backfillFrom(home);
    expect(r.env.ROOST_WORKER_LABEL).toBe("mike-m5-air");
    expect(r.env.ROOST_REACHABLE_ADDR).toBe("mike-m5-air.tail1234.ts.net");

    // The leak. Against the pre-fix implementation these two are set.
    expect(process.env.ROOST_WORKER_LABEL).toBeUndefined();
    expect(process.env.ROOST_REACHABLE_ADDR).toBeUndefined();
    expect(process.env.ROOST_COORDINATOR_URL).toBeUndefined();
  });

  test("host A's identity does not reach host B in the same process", async () => {
    const a = fakeHost("m5-air", {
      plist: plistWith({
        ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
        ROOST_REACHABLE_ADDR: "mike-m5-air.tail1234.ts.net",
        ROOST_WORKER_LABEL: "mike-m5-air",
      }),
    });
    const b = fakeHost("ovh1", {
      unit: unitWith({
        ROOST_REACHABLE_ADDR: "ovh1-8c32g.tail1234.ts.net",
        ROOST_WORKER_LABEL: "ovh1-8c32g",
      }),
    });

    const first = await backfillFrom(a);
    expect(first.filled).toContain("ROOST_WORKER_LABEL");

    const second = await backfillFrom(b);
    expect(second.env.ROOST_WORKER_LABEL).toBe("ovh1-8c32g");
    expect(second.env.ROOST_REACHABLE_ADDR).toBe("ovh1-8c32g.tail1234.ts.net");
    expect(second.filled.sort()).toEqual(["ROOST_REACHABLE_ADDR", "ROOST_WORKER_LABEL"]);
  });

  test("a host with no service definition cannot inherit the previous host's values", async () => {
    const a = fakeHost("m5-air", {
      plist: plistWith({
        ROOST_REACHABLE_ADDR: "mike-m5-air.tail1234.ts.net",
        ROOST_WORKER_LABEL: "mike-m5-air",
      }),
    });
    const fresh = fakeHost("fresh-box", {});

    await backfillFrom(a);
    const second = await backfillFrom(fresh);
    expect(second.env).toEqual({});
    expect(second.filled).toEqual([]);
    // Without this the case is vacuous: the pre-fix implementation also
    // returns {} here — because A already leaked into the ambient env, which
    // is exactly the state the fresh box would then be installed with.
    expect(process.env.ROOST_WORKER_LABEL).toBeUndefined();
    expect(process.env.ROOST_REACHABLE_ADDR).toBeUndefined();
  });
});

describe("_backfillEnvFromPlist — parsing", () => {
  test("reads a macOS LaunchAgent plist", async () => {
    const home = fakeHost("mac", {
      plist: plistWith({
        ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
        ROOST_WORKER_LABEL: "mike-m5-air",
      }),
    });
    const r = await backfillFrom(home);
    expect(r.env).toEqual({
      ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
      ROOST_WORKER_LABEL: "mike-m5-air",
    });
    expect(r.filled.sort()).toEqual(["ROOST_COORDINATOR_URL", "ROOST_WORKER_LABEL"]);
  });

  test("reads a systemd --user unit", async () => {
    const home = fakeHost("linux", {
      unit: unitWith({
        ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
        ROOST_WORKER_LABEL: "ovh1-8c32g",
      }),
    });
    const r = await backfillFrom(home);
    expect(r.env).toEqual({
      ROOST_COORDINATOR_URL: "https://coord.tail1234.ts.net:4102",
      ROOST_WORKER_LABEL: "ovh1-8c32g",
    });
    expect(r.filled.sort()).toEqual(["ROOST_COORDINATOR_URL", "ROOST_WORKER_LABEL"]);
  });

  test("both files present: the two syntaxes merge, neither parser eats the other's text", async () => {
    const home = fakeHost("both", {
      plist: plistWith({ ROOST_WORKER_LABEL: "from-plist" }),
      unit: unitWith({ ROOST_REACHABLE_ADDR: "from-unit.tail1234.ts.net" }),
    });
    const r = await backfillFrom(home);
    expect(r.env).toEqual({
      ROOST_REACHABLE_ADDR: "from-unit.tail1234.ts.net",
      ROOST_WORKER_LABEL: "from-plist",
    });
  });

  test("ignores non-ROOST keys", async () => {
    const home = fakeHost("mac", {
      plist: plistWith({ PATH: "/usr/bin", ROOST_WORKER_LABEL: "mike-m5-air" }),
    });
    const r = await backfillFrom(home);
    expect(r.env).toEqual({ ROOST_WORKER_LABEL: "mike-m5-air" });
  });
});

describe("_backfillEnvFromPlist — precedence and absence", () => {
  test("explicit per-host label and address still win", async () => {
    const home = fakeHost("mac", {
      plist: plistWith({
        ROOST_REACHABLE_ADDR: "installed.tail1234.ts.net",
        ROOST_WORKER_LABEL: "installed-label",
      }),
    });
    process.env.ROOST_REACHABLE_ADDR = "explicit.tail1234.ts.net";
    process.env.ROOST_WORKER_LABEL = "explicit-label";

    const r = await backfillFrom(home);
    expect(_resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env)).toBe("explicit.tail1234.ts.net");
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", r.env)).toBe("explicit-label");
    expect(r.env.ROOST_REACHABLE_ADDR).toBeUndefined();
    expect(r.env.ROOST_WORKER_LABEL).toBeUndefined();
    expect(process.env.ROOST_REACHABLE_ADDR).toBe("explicit.tail1234.ts.net");
    expect(process.env.ROOST_WORKER_LABEL).toBe("explicit-label");
  });

  test("an installed systemd or plist coordinator URL beats stale ambient state", async () => {
    process.env.ROOST_COORDINATOR_URL = "https://stale-shell.tail1234.ts.net:4102";
    const hosts = [
      fakeHost("mac", {
        plist: plistWith({ ROOST_COORDINATOR_URL: "https://mac-coord.tail1234.ts.net:4102" }),
      }),
      fakeHost("linux", {
        unit: unitWith({ ROOST_COORDINATOR_URL: "https://linux-coord.tail1234.ts.net:4102" }),
      }),
    ];

    for (const [index, home] of hosts.entries()) {
      const r = await backfillFrom(home);
      const expected = index === 0
        ? "https://mac-coord.tail1234.ts.net:4102"
        : "https://linux-coord.tail1234.ts.net:4102";
      expect(r.env.ROOST_COORDINATOR_URL).toBe(expected);
      expect(r.filled).toContain("ROOST_COORDINATOR_URL");
      expect(_resolveDeployEnvValue("ROOST_COORDINATOR_URL", r.env)).toBe(expected);
      expect(process.env.ROOST_COORDINATOR_URL).toBe("https://stale-shell.tail1234.ts.net:4102");
    }
  });

  test("missing service definition returns empty without throwing", async () => {
    const r = await backfillFrom(join(root, "does-not-exist"));
    expect(r).toEqual({ env: {}, filled: [] });
  });

  test("unreadable service definition (a directory where the plist should be) returns empty", async () => {
    const home = join(root, "weird");
    fs.mkdirSync(join(home, "Library/LaunchAgents/com.roost.worker-v2.plist"), { recursive: true });
    const r = await backfillFrom(home);
    expect(r).toEqual({ env: {}, filled: [] });
  });

  test("a service definition with no ROOST_* keys returns empty", async () => {
    const home = fakeHost("bare", { plist: "<plist><dict></dict></plist>", unit: "[Service]\n" });
    const r = await backfillFrom(home);
    expect(r).toEqual({ env: {}, filled: [] });
  });
});
