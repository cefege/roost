import { afterEach, describe, expect, test } from "bun:test";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "../../..");
const INSTALLER = join(ROOT, "apps/coord/scripts/install.sh");
const cleanups: string[] = [];

afterEach(() => {
  for (const path of cleanups.splice(0)) rmSync(path, { recursive: true, force: true });
});

function executable(path: string, source: string): void {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

function fixture(): { dir: string; unit: string; log: string; env: Record<string, string> } {
  const dir = mkdtempSync(join(tmpdir(), "roost-install-public-"));
  cleanups.push(dir);
  const bin = join(dir, "bin");
  mkdirSync(bin);
  const log = join(dir, "commands.log");
  executable(join(bin, "uname"), "#!/bin/sh\nprintf 'Linux\\n'\n");
  executable(join(bin, "loginctl"), "#!/bin/sh\nexit 0\n");
  executable(join(bin, "systemctl"), `#!/bin/sh
printf '%s\\n' "$*" >> "$ROOST_TEST_LOG"
case "$*" in
  *"is-active"*) [ "${"$"}{ROOST_TEST_PROCESS_DEAD:-0}" = 1 ] && exit 1 ;;
esac
exit 0
`);
  executable(join(bin, "stat"), `#!/bin/sh
if [ "$1" = "-c" ]; then printf '640\\n'; else exec /usr/bin/stat "$@"; fi
`);
  executable(join(bin, "tailscale"), `#!/bin/sh
printf 'tailscale %s\\n' "$*" >> "$ROOST_TEST_LOG"
if [ "$1 $2" = "serve --bg" ] && [ "${"$"}{ROOST_TEST_TAILSCALE_FAIL:-0}" = 1 ]; then exit 1; fi
if [ "$1 $2" = "serve status" ]; then printf 'https://tailnet.example -> http://127.0.0.1:4103\\n'; fi
exit 0
`);
  executable(join(bin, "curl"), `#!/bin/sh
headers=""
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    -D) headers="$2"; shift 2 ;;
    -o) output="$2"; shift 2 ;;
    -w|-X|-H|--data) shift 2 ;;
    -*) shift ;;
    *) url="$1"; shift ;;
  esac
done
case "$url" in
  */roost.v1.CoordinatorService/MiscHealth)
    status="${"$"}{ROOST_TEST_HEALTH_STATUS:-200}"
    if [ -n "$output" ]; then
      if [ "${"$"}{ROOST_TEST_HEALTH_BODY+x}" = x ]; then
        printf '%s' "$ROOST_TEST_HEALTH_BODY" > "$output"
      else
        printf '%s' '{"ok":true}' > "$output"
      fi
    fi
    ;;
  *)
    status="${"$"}{ROOST_TEST_CURL_STATUS:-401}"
    if [ "$status" = 401 ] && [ -n "$headers" ]; then
      printf 'HTTP/1.1 401 Unauthorized\\r\\nX-Roost-Auth-Layer: access\\r\\n\\r\\n' > "$headers"
    fi
    ;;
esac
printf '%s' "$status"
`);
  const unit = join(dir, "coord.service");
  const env: Record<string, string> = {
    ...process.env,
    PATH: `${bin}:/usr/bin:/bin`,
    HOME: dir,
    USER: "tester",
    BUN_BIN: "/usr/bin/true",
    ROOST_REPO_ROOT: ROOT,
    ROOST_SKIP_ENV_LOCAL: "1",
    ROOST_COORD_UNIT: unit,
    ROOST_COORD_DATA_DIR: join(dir, "data"),
    ROOST_COORD_LOG_DIR: join(dir, "logs"),
    ROOST_COORD_LOGROTATE_CONF: join(dir, "logrotate.conf"),
    ROOST_FRONTED: "1",
    ROOST_PUBLIC_BIND: "127.0.0.1:4104",
    ROOST_WEB_PUBLIC_URL: "https://roost.example.com",
    ROOST_CF_ACCESS_TEAM_DOMAIN: "example.cloudflareaccess.com",
    ROOST_CF_ACCESS_AUD: "a".repeat(64),
    ROOST_INSTALL_READY_ATTEMPTS: "1",
    ROOST_INSTALL_READY_INTERVAL_SECS: "0",
    ROOST_TEST_LOG: log,
  };
  return { dir, unit, log, env };
}

function install(env: Record<string, string>): ReturnType<typeof Bun.spawnSync> {
  return Bun.spawnSync(["bash", INSTALLER, "install"], { cwd: ROOT, env });
}

describe("public coordinator service install", () => {
  test("success requires liveness, tailscale mapping, and Access-marked 401 readiness", () => {
    const { unit, env } = fixture();
    const result = install(env);
    expect(result.exitCode, result.stderr?.toString()).toBe(0);
    expect(result.stdout?.toString()).toContain("Coord v2 ready");
    const service = readFileSync(unit, "utf8");
    expect(service).toContain("Environment=ROOST_PUBLIC_BIND=127.0.0.1:4104");
    expect(service).toContain("Environment=ROOST_CF_ACCESS_TEAM_DOMAIN=example.cloudflareaccess.com");
    expect(service).toContain("Environment=ROOST_FRONTED=1");
    expect(service).toContain("Environment=ROOST_COORD_LOOPBACK_PORT=4103");
  });

  test("wrong readiness, immediate exit, and tailscale failure restore an existing unit", () => {
    const failures: Array<Record<string, string>> = [
      { ROOST_TEST_CURL_STATUS: "200" },
      { ROOST_TEST_HEALTH_STATUS: "503" },
      { ROOST_TEST_HEALTH_BODY: "{\"ok\":false}" },
      { ROOST_TEST_PROCESS_DEAD: "1" },
      { ROOST_TEST_TAILSCALE_FAIL: "1" },
    ];
    for (const failure of failures) {
      const { unit, log, env } = fixture();
      const previous = Buffer.from("[Unit]\nDescription=exact prior bytes\n");
      writeFileSync(unit, previous);
      chmodSync(unit, 0o640);
      const result = install({ ...env, ...failure });
      expect(result.exitCode, `${JSON.stringify(failure)}\n${result.stdout?.toString()}\n${result.stderr?.toString()}`).not.toBe(0);
      expect(result.stdout?.toString()).not.toContain("Coord v2 ready");
      expect(readFileSync(unit)).toEqual(previous);
      expect(statSync(unit).mode & 0o777).toBe(0o640);
      expect(readFileSync(log, "utf8")).toContain("restart roost-coord.service");
    }
  });

  test("failed fresh install removes its generated service definition", () => {
    const { unit, env } = fixture();
    const result = install({ ...env, ROOST_TEST_CURL_STATUS: "502" });
    expect(result.exitCode).not.toBe(0);
    expect(existsSync(unit)).toBe(false);
    expect(result.stdout?.toString()).not.toContain("Coord v2 ready");
  });
});
