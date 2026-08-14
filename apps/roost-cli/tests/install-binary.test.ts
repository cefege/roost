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
import { delimiter, join, resolve } from "node:path";

const ROOT = resolve(import.meta.dir, "../../..");
const INSTALLER = join(ROOT, "install-binary.sh");
const cleanups: string[] = [];

type CurlMode = "valid" | "malformed-digest" | "missing-digest" | "altered-binary";

function executable(path: string, source: string): void {
  writeFileSync(path, source);
  chmodSync(path, 0o755);
}

function runInstaller(
  os: string,
  arch: string,
  options: { mode?: CurlMode; existing?: string; local?: boolean; localBin?: string } = {},
) {
  const dir = mkdtempSync(join(tmpdir(), "roost-install-binary-"));
  cleanups.push(dir);
  const bin = join(dir, "fake-bin");
  const destDir = join(dir, "dest");
  const destination = join(destDir, "roost");
  const curlLog = join(dir, "curl.log");
  const payload = join(dir, "release-binary");
  mkdirSync(bin);
  if (options.existing !== undefined) {
    mkdirSync(destDir);
    writeFileSync(destination, options.existing);
    chmodSync(destination, 0o755);
  }
  executable(payload, "#!/bin/sh\nprintf 'test-version\\n'\n");

  executable(join(bin, "uname"), `#!/bin/sh\ncase "$1" in\n  -s) printf '%s\\n' "$FAKE_OS" ;;\n  -m) printf '%s\\n' "$FAKE_ARCH" ;;\n  *) exit 2 ;;\nesac\n`);
  executable(join(bin, "tailscale"), "#!/bin/sh\nexit 0\n");
  executable(join(bin, "shasum"), `#!/bin/sh\nlast=''\nfor arg in "$@"; do last="$arg"; done\nexec /usr/bin/sha256sum "$last"\n`);
  executable(join(bin, "sha256sum"), "#!/bin/sh\nexec /usr/bin/sha256sum \"$@\"\n");
  executable(join(bin, "curl"), `#!/bin/sh\nset -eu\nurl=''\nout=''\nwhile [ "$#" -gt 0 ]; do\n  case "$1" in\n    -o) out="$2"; shift 2 ;;\n    -*) shift ;;\n    *) url="$1"; shift ;;\n  esac\ndone\nprintf '%s\\n' "$url" >> "$CURL_LOG"\ncase "$url" in\n  *.sha256)\n    case "$CURL_MODE" in\n      malformed-digest) printf 'NOT-A-DIGEST\\n' > "$out" ;;\n      missing-digest) exit 22 ;;\n      *) set -- $(/usr/bin/sha256sum "$PAYLOAD"); printf '%s\\n' "$1" > "$out" ;;\n    esac\n    ;;\n  *)\n    if [ "$CURL_MODE" = altered-binary ]; then\n      printf '#!/bin/sh\\nprintf truncated\\n' > "$out"\n    else\n      cp "$PAYLOAD" "$out"\n    fi\n    ;;\nesac\n`);

  const result = Bun.spawnSync(["bash", INSTALLER], {
    cwd: ROOT,
    env: {
      ...process.env,
      PATH: `${bin}${delimiter}${process.env.PATH ?? ""}`,
      FAKE_OS: os,
      FAKE_ARCH: arch,
      CURL_LOG: curlLog,
      CURL_MODE: options.mode ?? "valid",
      PAYLOAD: payload,
      ROOST_BIN_DIR: destDir,
      ...(options.local || options.localBin
        ? { ROOST_LOCAL_BIN: options.local ? payload : options.localBin! }
        : {}),
    },
  });
  return { result, curlLog, destination, payload };
}

afterEach(() => {
  for (const dir of cleanups.splice(0)) rmSync(dir, { recursive: true, force: true });
});

describe("install-binary architecture selection", () => {
  for (const [os, arch, asset] of [
    ["Darwin", "arm64", "roost"],
    ["Darwin", "x86_64", "roost-darwin-x64"],
    ["Linux", "x86_64", "roost-linux-x64"],
    ["Linux", "aarch64", "roost-linux-arm64"],
  ] as const) {
    test(`${os}/${arch} downloads and verifies ${asset}`, () => {
      const { result, curlLog, destination, payload } = runInstaller(os, arch);
      expect(result.exitCode, `${result.stdout.toString()}\n${result.stderr.toString()}`).toBe(0);
      expect(readFileSync(curlLog, "utf8").trim().split("\n")).toEqual([
        `https://github.com/cefege/roost/releases/latest/download/${asset}`,
        `https://github.com/cefege/roost/releases/latest/download/${asset}.sha256`,
      ]);
      expect(readFileSync(destination)).toEqual(readFileSync(payload));
      expect(statSync(destination).mode & 0o777).toBe(0o755);
    });
  }

  test("unsupported tuples fail before downloading or creating the destination", () => {
    const { result, curlLog, destination } = runInstaller("Darwin", "aarch64");
    expect(result.exitCode).not.toBe(0);
    expect(existsSync(curlLog)).toBe(false);
    expect(existsSync(destination)).toBe(false);
    expect(result.stderr.toString()).toContain("no prebuilt roost binary for Darwin/aarch64");
  });
});

describe("install-binary verified atomic replacement", () => {
  for (const mode of ["malformed-digest", "missing-digest", "altered-binary"] as const) {
    test(`${mode} fails and preserves an existing executable`, () => {
      const before = `existing-${mode}\n`;
      const { result, destination } = runInstaller("Linux", "x86_64", { mode, existing: before });
      expect(result.exitCode).not.toBe(0);
      expect(readFileSync(destination, "utf8")).toBe(before);
    });
  }

  test("local binaries use the same candidate-and-rename path", () => {
    const { result, curlLog, destination, payload } = runInstaller("Linux", "x86_64", {
      existing: "old\n",
      local: true,
    });
    expect(result.exitCode, `${result.stdout.toString()}\n${result.stderr.toString()}`).toBe(0);
    expect(existsSync(curlLog)).toBe(false);
    expect(readFileSync(destination)).toEqual(readFileSync(payload));
    expect(statSync(destination).mode & 0o777).toBe(0o755);
  });

  test("a failed local copy preserves an existing executable", () => {
    const { result, destination } = runInstaller("Linux", "x86_64", {
      existing: "old\n",
      localBin: "/missing",
    });
    expect(result.exitCode).not.toBe(0);
    expect(readFileSync(destination, "utf8")).toBe("old\n");
  });
});
