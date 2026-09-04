import { afterEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "../../..");
const INSTALLER = join(ROOT, "apps/coord/scripts/install.sh");
const cleanups: string[] = [];

function executable(path: string, source: string): void {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

function fixture(platform: "Linux" | "Darwin") {
  const root = mkdtempSync(join(tmpdir(), "roost-coord-installer-"));
  cleanups.push(root);
  const bin = join(root, "bin");
  const home = join(root, "home");
  const definition = join(root, platform === "Linux" ? "coord.service" : "coord.plist");
  mkdirSync(bin);
  mkdirSync(home);
  executable(join(bin, "uname"), `#!/bin/sh\nprintf '%s\\n' '${platform}'\n`);

  const env: Record<string, string> = {
    PATH: `${bin}:/usr/bin:/bin`,
    HOME: home,
    USER: "roost-test",
    BUN_BIN: "/usr/bin/true",
    ROOST_EXEC_BIN: "/usr/bin/true",
    ROOST_REPO_ROOT: ROOT,
    ROOST_SKIP_ENV_LOCAL: "1",
    ROOST_COORD_UNIT: definition,
    ROOST_COORD_PLIST: definition,
    ROOST_COORD_DATA_DIR: join(root, "data"),
    ROOST_COORD_LOG_DIR: join(root, "logs"),
    ROOST_COORD_LOGROTATE_CONF: join(root, "logrotate.conf"),
    ROOST_GIT_SHA: "test-sha",
  };
  return { root, bin, definition, env };
}

function envValue(
  definition: string,
  platform: "Linux" | "Darwin",
  key: string,
): string | null {
  if (platform === "Linux") {
    const prefix = `Environment="${key}=`;
    const line = definition.split("\n").find((candidate) => candidate.startsWith(prefix));
    return line ? line.slice(prefix.length, -1) : null;
  }
  const match = new RegExp(`<key>${key}</key>\\s*<string>([^<]*)</string>`).exec(definition);
  return match?.[1] ?? null;
}

function writeDefinition(
  platform: "Linux" | "Darwin",
  overrides: Record<string, string>,
): string {
  const { definition, env } = fixture(platform);
  const result = Bun.spawnSync(["bash", INSTALLER, "write-plist"], {
    cwd: ROOT,
    env: { ...env, ...overrides },
  });
  expect(result.exitCode, result.stderr.toString()).toBe(0);
  return readFileSync(definition, "utf8");
}

afterEach(() => {
  for (const path of cleanups.splice(0)) rmSync(path, { recursive: true, force: true });
});

describe.skipIf(process.platform === "win32")("POSIX coordinator installer endpoint modes", () => {
  test("direct definitions persist the exact endpoint contract on arbitrary ports", () => {
    for (const platform of ["Linux", "Darwin"] as const) {
      const definition = writeDefinition(platform, {
        ROOST_FRONTED: "0",
        ROOST_COORDINATOR_BIND: "0.0.0.0:18443",
        ROOST_COORDINATOR_PUBLIC_URL: "https://coord.example.test:18443",
        ROOST_TLS_CERT_PATH: "/var/lib/roost/tls/fullchain.pem",
        ROOST_TLS_KEY_PATH: "/var/lib/roost/tls/privkey.pem",
        // Direct mode must ignore automatic-mode settings even when ambient.
        ROOST_COORD_LOOPBACK_PORT: "19998",
        ROOST_TAILNET_HTTPS_PORT: "19999",
      });

      expect(envValue(definition, platform, "ROOST_FRONTED")).toBe("0");
      expect(envValue(definition, platform, "ROOST_COORDINATOR_BIND"))
        .toBe("0.0.0.0:18443");
      expect(envValue(definition, platform, "ROOST_COORDINATOR_PUBLIC_URL"))
        .toBe("https://coord.example.test:18443");
      expect(envValue(definition, platform, "ROOST_TLS_CERT_PATH"))
        .toBe("/var/lib/roost/tls/fullchain.pem");
      expect(envValue(definition, platform, "ROOST_TLS_KEY_PATH"))
        .toBe("/var/lib/roost/tls/privkey.pem");
      expect(envValue(definition, platform, "ROOST_COORD_LOOPBACK_PORT")).toBeNull();
      expect(envValue(definition, platform, "ROOST_TAILNET_HTTPS_PORT")).toBeNull();
      expect(envValue(definition, platform, "ROOST_TRUST_PROXY")).toBeNull();
    }
  });

  test("automatic definitions retain loopback and Tailscale Serve settings", () => {
    for (const platform of ["Linux", "Darwin"] as const) {
      const definition = writeDefinition(platform, {
        ROOST_FRONTED: "1",
        ROOST_COORD_LOOPBACK_PORT: "14103",
        ROOST_TAILNET_HTTPS_PORT: "14102",
        ROOST_TLS_CERT_PATH: "/unused/cert.pem",
        ROOST_TLS_KEY_PATH: "/unused/key.pem",
      });

      expect(envValue(definition, platform, "ROOST_FRONTED")).toBe("1");
      expect(envValue(definition, platform, "ROOST_COORDINATOR_BIND"))
        .toBe("127.0.0.1:14103");
      expect(envValue(definition, platform, "ROOST_COORD_LOOPBACK_PORT")).toBe("14103");
      expect(envValue(definition, platform, "ROOST_TAILNET_HTTPS_PORT")).toBe("14102");
      expect(envValue(definition, platform, "ROOST_TRUST_PROXY")).toBe("1");
      expect(envValue(definition, platform, "ROOST_TLS_CERT_PATH")).toBeNull();
      expect(envValue(definition, platform, "ROOST_TLS_KEY_PATH")).toBeNull();
    }
  });

  test("direct install never invokes Tailscale", () => {
    const { root, bin, definition, env } = fixture("Linux");
    const tailscaleLog = join(root, "tailscale.log");
    executable(join(bin, "loginctl"), "#!/bin/sh\nexit 0\n");
    executable(join(bin, "systemctl"), "#!/bin/sh\nexit 0\n");
    executable(join(bin, "logrotate"), "#!/bin/sh\nexit 0\n");
    executable(
      join(bin, "tailscale"),
      "#!/bin/sh\nprintf 'called\\n' >> \"$TAILSCALE_LOG\"\nexit 73\n",
    );
    executable(
      join(bin, "curl"),
      "#!/bin/sh\nout=''\nwhile [ \"$#\" -gt 0 ]; do\n  case \"$1\" in\n    -o) out=\"$2\"; shift 2 ;;\n    -w|-X|-H|--data) shift 2 ;;\n    *) shift ;;\n  esac\ndone\nprintf '{\"gitSha\":\"test-sha\"}' > \"$out\"\nprintf '200'\n",
    );

    const result = Bun.spawnSync(["bash", INSTALLER, "install"], {
      cwd: ROOT,
      env: {
        ...env,
        TAILSCALE_LOG: tailscaleLog,
        ROOST_FRONTED: "0",
        ROOST_COORDINATOR_BIND: "0.0.0.0:28443",
        ROOST_COORDINATOR_PUBLIC_URL: "https://coord.example.test:28443",
        ROOST_TLS_CERT_PATH: join(root, "fullchain.pem"),
        ROOST_TLS_KEY_PATH: join(root, "privkey.pem"),
        ROOST_INSTALL_READY_ATTEMPTS: "1",
        ROOST_INSTALL_READY_INTERVAL_SECS: "0",
      },
    });

    expect(result.exitCode, result.stderr.toString()).toBe(0);
    expect(existsSync(definition)).toBe(true);
    expect(existsSync(tailscaleLog)).toBe(false);
    expect(result.stdout.toString()).toContain(
      "direct HTTPS) - https://coord.example.test:28443 (bind 0.0.0.0:28443)",
    );
    expect(result.stdout.toString()).not.toContain("tailnet https://");
  });
});
