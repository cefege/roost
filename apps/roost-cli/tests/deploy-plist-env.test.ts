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
import { _quoteRemoteShell, _remoteCertCommand, _selectMacCertificateFqdn } from "../src/deploy.ts";
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
  // Keep ambient identity deterministic; precedence tests opt in explicitly.
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

describe("_quoteRemoteShell", () => {
  test("preserves remote-shell metacharacters without executing them", () => {
    const commandSubstitution = join(root, "command-substitution");
    const backtickSubstitution = join(root, "backtick-substitution");
    const value = `$(touch ${commandSubstitution}) \`touch ${backtickSubstitution}\` $HOME O'Reilly`;
    const proc = Bun.spawnSync([
      "bash",
      "-c",
      `VALUE=${_quoteRemoteShell(value)}; printf '%s' "$VALUE"`,
    ]);

    expect(proc.exitCode).toBe(0);
    expect(proc.stdout.toString()).toBe(value);
    expect(fs.existsSync(commandSubstitution)).toBe(false);
    expect(fs.existsSync(backtickSubstitution)).toBe(false);
  });

  test("certificate command preserves the tailscale failure status", () => {
    const bin = join(root, "bin");
    const tailscale = join(bin, "tailscale");
    fs.mkdirSync(bin, { recursive: true });
    fs.writeFileSync(tailscale, "#!/usr/bin/env bash\nexit 23\n");
    fs.chmodSync(tailscale, 0o700);

    const proc = Bun.spawnSync(["bash", "-c", _remoteCertCommand("worker.example")], {
      env: {
        ...process.env,
        HOME: join(root, "remote-home"),
        PATH: `${bin}:${process.env.PATH ?? "/usr/bin:/bin"}`,
      },
    });
    expect(proc.exitCode).toBe(23);
  });
});

describe("worker installer environment precedence", () => {
  test("explicit enrolled identity wins while absent values use host-local defaults", () => {
    const repo = join(root, "repo");
    const scriptDir = join(repo, "apps/worker/scripts");
    const installer = join(scriptDir, "install.sh");
    const home = join(root, "home");
    const unit = join(home, ".config/systemd/user/test-worker.service");
    fs.mkdirSync(scriptDir, { recursive: true });
    fs.mkdirSync(home, { recursive: true });
    fs.copyFileSync(join(import.meta.dir, "../../worker/scripts/install.sh"), installer);
    fs.chmodSync(installer, 0o700);
    fs.writeFileSync(
      join(repo, ".env.local"),
      [
        "ROOST_COORDINATOR_URL=https://stale.example:4102",
        "ROOST_WORKER_LABEL=stale-worker",
        "ROOST_REACHABLE_ADDR=stale.example",
        "ROOST_WORKER_MEMORY_HIGH=4G",
        "",
      ].join("\n"),
    );
    const env = { ...process.env };
    delete env.ROOST_WORKER_MEMORY_HIGH;
    Object.assign(env, {
      HOME: home,
      BUN_BIN: "/bin/true",
      ROOST_COORDINATOR_URL: "https://current.example:4102",
      ROOST_WORKER_LABEL: "current-worker",
      ROOST_REACHABLE_ADDR: "current.example",
      ROOST_WORKER_AGENT_LABEL: "test-worker",
      ROOST_WORKER_UNIT: unit,
      ROOST_WORKER_DATA_DIR: join(root, "data"),
      ROOST_WORKER_LOG_DIR: join(root, "logs"),
    });

    const proc = Bun.spawnSync(["bash", installer, "write-plist"], { env });
    expect(proc.exitCode).toBe(0);
    const definition = fs.readFileSync(unit, "utf8");
    expect(definition).toContain("Environment=ROOST_COORDINATOR_URL=https://current.example:4102");
    expect(definition).toContain("Environment=ROOST_WORKER_LABEL=current-worker");
    expect(definition).toContain("Environment=ROOST_REACHABLE_ADDR=current.example");
    expect(definition).toContain("MemoryHigh=4G");
    expect(definition).not.toContain("stale");
  });
});

describe("_backfillEnvFromPlist — precedence and absence", () => {
  test("an enrolled Mac keeps its installed identity over ambient coordinator-host identity", async () => {
    process.env.ROOST_COORDINATOR_URL = "https://coord-host.tail1234.ts.net:4102";
    process.env.ROOST_REACHABLE_ADDR = "coord-host.tail1234.ts.net";
    process.env.ROOST_WORKER_LABEL = "coord-host";
    const home = fakeHost("mac-worker", {
      plist: plistWith({
        ROOST_COORDINATOR_URL: "https://enrolled-coord.tail1234.ts.net:4102",
        ROOST_REACHABLE_ADDR: "mac-worker.tail1234.ts.net",
        ROOST_WORKER_LABEL: "mac-worker",
      }),
    });

    const r = await backfillFrom(home);
    expect(r.env).toEqual({
      ROOST_COORDINATOR_URL: "https://enrolled-coord.tail1234.ts.net:4102",
      ROOST_REACHABLE_ADDR: "mac-worker.tail1234.ts.net",
      ROOST_WORKER_LABEL: "mac-worker",
    });
    expect(r.filled.sort()).toEqual([
      "ROOST_COORDINATOR_URL",
      "ROOST_REACHABLE_ADDR",
      "ROOST_WORKER_LABEL",
    ]);

    const resolvedCoordinatorUrl = _resolveDeployEnvValue("ROOST_COORDINATOR_URL", r.env);
    const resolvedWorkerLabel = _resolveDeployEnvValue("ROOST_WORKER_LABEL", r.env);
    const resolvedReachableAddr = _resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env);
    expect(resolvedCoordinatorUrl).toBe("https://enrolled-coord.tail1234.ts.net:4102");
    expect(resolvedWorkerLabel).toBe("mac-worker");
    expect(resolvedReachableAddr).toBe("mac-worker.tail1234.ts.net");
    expect(_selectMacCertificateFqdn("mac-worker", resolvedReachableAddr, "wrong-tail.ts.net"))
      .toBe(resolvedReachableAddr);

    expect(process.env.ROOST_COORDINATOR_URL).toBe("https://coord-host.tail1234.ts.net:4102");
    expect(process.env.ROOST_REACHABLE_ADDR).toBe("coord-host.tail1234.ts.net");
    expect(process.env.ROOST_WORKER_LABEL).toBe("coord-host");
  });

  test("an enrolled Linux worker keeps its installed identity over ambient coordinator-host identity", async () => {
    process.env.ROOST_COORDINATOR_URL = "https://coord-host.tail1234.ts.net:4102";
    process.env.ROOST_REACHABLE_ADDR = "coord-host.tail1234.ts.net";
    process.env.ROOST_WORKER_LABEL = "coord-host";
    const home = fakeHost("linux-worker", {
      unit: unitWith({
        ROOST_COORDINATOR_URL: "https://enrolled-coord.tail1234.ts.net:4102",
        ROOST_REACHABLE_ADDR: "linux-worker.tail1234.ts.net",
        ROOST_WORKER_LABEL: "linux-worker",
      }),
    });

    const r = await backfillFrom(home);
    expect(_resolveDeployEnvValue("ROOST_COORDINATOR_URL", r.env))
      .toBe("https://enrolled-coord.tail1234.ts.net:4102");
    expect(_resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env))
      .toBe("linux-worker.tail1234.ts.net");
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", r.env)).toBe("linux-worker");
    expect(r.filled.sort()).toEqual([
      "ROOST_COORDINATOR_URL",
      "ROOST_REACHABLE_ADDR",
      "ROOST_WORKER_LABEL",
    ]);
  });

  test("a fresh target falls back to ambient deploy values", async () => {
    process.env.ROOST_COORDINATOR_URL = "https://coord-host.tail1234.ts.net:4102";
    process.env.ROOST_REACHABLE_ADDR = "fresh-mac.tail1234.ts.net";
    process.env.ROOST_WORKER_LABEL = "fresh-mac";
    const r = await backfillFrom(fakeHost("fresh-mac", {}));

    expect(r).toEqual({ env: {}, filled: [] });
    expect(_resolveDeployEnvValue("ROOST_COORDINATOR_URL", r.env))
      .toBe("https://coord-host.tail1234.ts.net:4102");
    expect(_resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env))
      .toBe("fresh-mac.tail1234.ts.net");
    expect(_resolveDeployEnvValue("ROOST_WORKER_LABEL", r.env)).toBe("fresh-mac");
    const resolvedReachableAddr = _resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env);
    expect(_selectMacCertificateFqdn("fresh-mac", resolvedReachableAddr, "tail1234.ts.net"))
      .toBe("fresh-mac.tail1234.ts.net");
  });

  test("a fresh Mac derives its certificate FQDN when reachable address is absent", async () => {
    process.env.ROOST_COORDINATOR_URL = "https://coord-host.tail1234.ts.net:4102";
    const r = await backfillFrom(fakeHost("fresh-mac", {}));
    const resolvedReachableAddr = _resolveDeployEnvValue("ROOST_REACHABLE_ADDR", r.env);

    expect(resolvedReachableAddr).toBeUndefined();
    expect(_selectMacCertificateFqdn("fresh-mac", resolvedReachableAddr, "tail1234.ts.net"))
      .toBe("fresh-mac.tail1234.ts.net");
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
