// POSIX installer tests: quickstart endpoint selection composed with the
// shipping service writers, plus the dist-path confinement both the coordinator
// and worker installers apply. Fake native commands keep activation hermetic
// while preserving the exact launchd and systemd boundaries.
import { afterEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import {
  coordinatorEnvironmentForQuickstart,
  resolveQuickstartEndpoint,
} from "../src/quickstart-endpoint.ts";

const ROOT = resolve(import.meta.dir, "../../..");
const INSTALLER = join(ROOT, "apps/coord/scripts/install.sh");
const WORKER_INSTALLER = join(ROOT, "apps/worker/scripts/install.sh");
const cleanups: string[] = [];

function executable(path: string, source: string): void {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

/** Answers the installer's readiness probe with a coordinator identity. */
const FAKE_CURL = "#!/bin/sh\nout=''\nwhile [ \"$#\" -gt 0 ]; do\n"
  + "  case \"$1\" in\n    -o) out=\"$2\"; shift 2 ;;\n"
  + "    -w|-X|-H|--data) shift 2 ;;\n    *) shift ;;\n  esac\ndone\n"
  + "printf '{\"gitSha\":\"test-sha\"}' > \"$out\"\nprintf '200'\n";

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

describe.skipIf(process.platform === "win32")("POSIX coordinator installer endpoint", () => {
  test("quickstart's explicit front door persists trusted proxy and exact local CORS", () => {
    for (const platform of ["Linux", "Darwin"] as const) {
      const endpointPlatform = platform === "Linux" ? "linux" : "darwin";
      const endpoint = resolveQuickstartEndpoint(
        ["--coordinator-url", "https://dash.example.test"],
        endpointPlatform,
      );
      const definition = writeDefinition(
        platform,
        coordinatorEnvironmentForQuickstart(endpoint),
      );

      expect(envValue(definition, platform, "ROOST_COORDINATOR_BIND"))
        .toBe("127.0.0.1:4103");
      expect(envValue(definition, platform, "ROOST_TRUST_PROXY")).toBe("1");
      expect(envValue(definition, platform, "ROOST_WEB_PUBLIC_URL"))
        .toBe("https://dash.example.test");
      expect(envValue(definition, platform, "ROOST_CORS_ALLOWED_ORIGINS"))
        .toBe("http://127.0.0.1:4103");
      expect(envValue(definition, platform, "ROOST_COORDINATOR_PUBLIC_URL")).toBe("");
    }
  });

  test("quickstart's bare profile persists direct local trust and clears public URLs", () => {
    for (const platform of ["Linux", "Darwin"] as const) {
      const endpointPlatform = platform === "Linux" ? "linux" : "darwin";
      const definition = writeDefinition(
        platform,
        coordinatorEnvironmentForQuickstart(resolveQuickstartEndpoint([], endpointPlatform)),
      );
      expect(envValue(definition, platform, "ROOST_TRUST_PROXY")).toBe("0");
      expect(envValue(definition, platform, "ROOST_WEB_PUBLIC_URL")).toBe("");
      expect(envValue(definition, platform, "ROOST_COORDINATOR_PUBLIC_URL")).toBe("");
      expect(envValue(definition, platform, "ROOST_CORS_ALLOWED_ORIGINS"))
        .toBe("http://127.0.0.1:4103");
    }
  });

  test("the loopback port knob moves the persisted bind", () => {
    const definition = writeDefinition("Linux", {
      ROOST_SKIP_ENV_LOCAL: "1",
      ROOST_COORD_LOOPBACK_PORT: "4207",
    });
    expect(envValue(definition, "Linux", "ROOST_COORDINATOR_BIND")).toBe("127.0.0.1:4207");
  });

  test("a non-loopback bind is refused rather than exposed", () => {
    for (const bind of ["0.0.0.0:4103", "10.0.0.4:4103", "[::]:4103", "127.0.0.1:0", "127.0.0.1:65536"]) {
      const { env, definition } = fixture("Linux");
      const result = Bun.spawnSync(["bash", INSTALLER, "write-plist"], {
        cwd: ROOT,
        env: { ...env, ROOST_SKIP_ENV_LOCAL: "1", ROOST_COORDINATOR_BIND: bind },
      });
      expect(result.exitCode).not.toBe(0);
      expect(result.stderr.toString()).toContain("must be 127.0.0.1:<port>");
      expect(existsSync(definition)).toBe(false);
    }
  });

  test("an invalid trust profile refuses before a service definition exists", () => {
    for (const trustProxy of ["", "true", "2", "-1"]) {
      const { env, definition } = fixture("Linux");
      const result = Bun.spawnSync(["bash", INSTALLER, "write-plist"], {
        cwd: ROOT,
        env: {
          ...env,
          ROOST_SKIP_ENV_LOCAL: "1",
          ROOST_TRUST_PROXY: trustProxy,
        },
      });
      expect(result.exitCode).not.toBe(0);
      expect(result.stderr.toString()).toContain("ROOST_TRUST_PROXY must be 0 or 1");
      expect(existsSync(definition)).toBe(false);
    }
  });

  test("install proves readiness on the loopback bind and reports the front door", () => {
    const { root, bin, definition, env } = fixture("Linux");
    // A stale checkout-local endpoint must not replace the selected one.
    writeFileSync(join(root, ".env.local"), [
      "ROOST_COORDINATOR_BIND=0.0.0.0:19443",
      "ROOST_WEB_PUBLIC_URL=https://stale.example.test",
      "",
    ].join("\n"));
    executable(join(bin, "loginctl"), "#!/bin/sh\nexit 0\n");
    executable(join(bin, "systemctl"), "#!/bin/sh\nexit 0\n");
    executable(join(bin, "logrotate"), "#!/bin/sh\nexit 0\n");
    executable(join(bin, "curl"), FAKE_CURL);

    const endpoint = resolveQuickstartEndpoint(
      ["--coordinator-url", "https://fresh.example.test"],
      "linux",
    );
    const result = Bun.spawnSync(["bash", INSTALLER, "install"], {
      cwd: ROOT,
      env: {
        ...env,
        ROOST_REPO_ROOT: root,
        ...coordinatorEnvironmentForQuickstart(endpoint),
        ROOST_INSTALL_READY_ATTEMPTS: "1",
        ROOST_INSTALL_READY_INTERVAL_SECS: "0",
      },
    });

    expect(result.exitCode, result.stderr.toString()).toBe(0);
    const installed = readFileSync(definition, "utf8");
    expect(envValue(installed, "Linux", "ROOST_COORDINATOR_BIND")).toBe("127.0.0.1:4103");
    expect(envValue(installed, "Linux", "ROOST_WEB_PUBLIC_URL"))
      .toBe("https://fresh.example.test");
    expect(result.stdout.toString()).toContain(
      "Coord v2 ready - bind 127.0.0.1:4103, front door https://fresh.example.test",
    );
  });

  test("compiled worker installer does not require Bun on PATH", () => {
    const { root, definition, env } = fixture("Linux");
    const compiledEnv = { ...env };
    delete compiledEnv.BUN_BIN;
    const result = Bun.spawnSync(["bash", WORKER_INSTALLER, "write-plist"], {
      cwd: ROOT,
      env: {
        ...compiledEnv,
        ROOST_COORDINATOR_URL: "http://127.0.0.1:4103",
        ROOST_WORKER_UNIT: definition,
        ROOST_WORKER_DATA_DIR: join(root, "worker-data"),
        ROOST_WORKER_LOG_DIR: join(root, "worker-logs"),
      },
    });

    expect(result.exitCode, result.stderr.toString()).toBe(0);
    const installed = readFileSync(definition, "utf8");
    expect(envValue(installed, "Linux", "ROOST_EXEC_BIN")).toBe("/usr/bin/true");
    expect(result.stderr.toString()).not.toContain("bun not found");
  });

  // Both installers canonicalize with `pwd -P`, and macOS resolves
  // mkdtemp's /var/folders to /private/var/folders, so every expectation here
  // compares against the realpath rather than the temp name.
  test("a dist path from another service's release tree is refused, not stamped", () => {
    for (const platform of ["Linux", "Darwin"] as const) {
      const { root, env, definition } = fixture(platform);
      // The coordinator installer honors ROOST_REPO_ROOT (a staged release
      // deploy sets it); the worker installer derives its root from its own
      // location and cannot be told otherwise. Each falls back inside its own.
      for (const [installer, installRoot] of [
        [INSTALLER, realpathSync(root)],
        [WORKER_INSTALLER, realpathSync(ROOT)],
      ] as const) {
        // What a shell that just deployed the sibling service exports: a dist
        // inside THAT service's tree, deleted by its next settlement. Left
        // stamped here, this service answers 404 on every page.
        const sibling = realpathSync(mkdtempSync(join(tmpdir(), "roost-sibling-service-")));
        cleanups.push(sibling);
        const foreign = join(sibling, "releases", "r1", "apps", "web", "dist");
        mkdirSync(foreign, { recursive: true });
        writeFileSync(join(foreign, "index.html"), "<!doctype html>");

        const result = Bun.spawnSync(["bash", installer, "write-plist"], {
          cwd: ROOT,
          env: {
            ...env,
            ROOST_SKIP_ENV_LOCAL: "1",
            ROOST_COORDINATOR_URL: "https://dash.example.test",
            ROOST_WORKER_UNIT: definition,
            ROOST_WORKER_PLIST: definition,
            ROOST_WORKER_DATA_DIR: join(root, "worker-data"),
            ROOST_WORKER_LOG_DIR: join(root, "worker-logs"),
            ROOST_REPO_ROOT: root,
            ROOST_WEB_DIST_PATH: foreign,
          },
        });

        expect(result.exitCode, result.stderr.toString()).toBe(0);
        expect(result.stderr.toString()).toContain(`ignoring ROOST_WEB_DIST_PATH=${foreign}`);
        expect(envValue(readFileSync(definition, "utf8"), platform, "ROOST_WEB_DIST_PATH"))
          .toBe(join(installRoot, "apps", "web", "dist"));
      }
    }
  });

  test("a dist inside this install's own root is stamped as given", () => {
    const { root, env, definition } = fixture("Linux");
    // The staged-release deploy shape: the installer's own root IS the release.
    const own = join(realpathSync(root), "releases", "coord-r2", "apps", "web", "dist");
    mkdirSync(own, { recursive: true });

    const result = Bun.spawnSync(["bash", INSTALLER, "write-plist"], {
      cwd: ROOT,
      env: { ...env, ROOST_SKIP_ENV_LOCAL: "1", ROOST_REPO_ROOT: root, ROOST_WEB_DIST_PATH: own },
    });

    expect(result.exitCode, result.stderr.toString()).toBe(0);
    expect(envValue(readFileSync(definition, "utf8"), "Linux", "ROOST_WEB_DIST_PATH")).toBe(own);
  });
});
