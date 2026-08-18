// roost update contract: version-compare semantics, release-asset resolution
// (a Linux install must never download the Darwin binary and rename it over
// its own execPath), and orchestration (skip when no release / up to date,
// download-then-replace when newer) with injected deps — no network, no real
// binary swap.
import { afterEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, renameSync, rmSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import {
  fetchAndVerifyReleaseAsset,
  needsUpdate,
  releaseAssetName,
  runUpdate,
  update,
  type UpdateDeps,
} from "../src/update.ts";

describe("releaseAssetName", () => {
  test("darwin/arm64 keeps the legacy unsuffixed asset", () => {
    expect(releaseAssetName("darwin", "arm64")).toBe("roost");
  });
  test("darwin/x64 is explicit", () => {
    expect(releaseAssetName("darwin", "x64")).toBe("roost-darwin-x64");
  });
  test("linux resolves per arch", () => {
    expect(releaseAssetName("linux", "x64")).toBe("roost-linux-x64");
    expect(releaseAssetName("linux", "arm64")).toBe("roost-linux-arm64");
  });
  test("win32/x64 resolves to the packaged release archive", () => {
    expect(releaseAssetName("win32", "x64")).toBe("roost-windows-x64.zip");
  });
  test("unsupported platform throws rather than guessing", () => {
    expect(() => releaseAssetName("freebsd", "x64")).toThrow(
      "no prebuilt roost binary for freebsd/x64",
    );
  });
  test("unsupported architecture throws rather than guessing", () => {
    expect(() => releaseAssetName("linux", "riscv64")).toThrow(/no prebuilt roost binary/);
  });
});

describe("needsUpdate", () => {
  test("dev (from-source) is always behind a release", () => {
    expect(needsUpdate("dev", "v1.2.0")).toBe(true);
  });
  test("same base version → no update; build metadata (+sha) ignored", () => {
    expect(needsUpdate("1.2.0", "v1.2.0")).toBe(false);
    expect(needsUpdate("1.2.0+abc123", "v1.2.0")).toBe(false);
  });
  test("different version → update", () => {
    expect(needsUpdate("1.2.0", "v1.3.0")).toBe(true);
  });
  test("no release tag → no update", () => {
    expect(needsUpdate("1.2.0", "")).toBe(false);
  });
});

function makeDeps(over: { current?: string; latest?: string }) {
  const logs: string[] = [];
  const calls = { downloaded: null as string | null, replaced: null as string | null };
  const d: UpdateDeps = {
    currentVersion: over.current ?? "1.0.0",
    execPath: "/tmp/roost",
    log: (m) => { logs.push(m); },
    fetchLatestTag: async () => over.latest ?? "",
    downloadBinary: async (p) => { calls.downloaded = p; },
    replaceSelf: (p) => { calls.replaced = p; },
  };
  return { logs, calls, d };
}

describe("runUpdate orchestration", () => {
  test("no release → nothing downloaded or replaced", async () => {
    const h = makeDeps({ latest: "" });
    const r = await runUpdate(h.d);
    expect(r.updated).toBe(false);
    expect(h.calls.downloaded).toBeNull();
    expect(h.calls.replaced).toBeNull();
  });
  test("already up to date → no swap", async () => {
    const h = makeDeps({ current: "1.0.0", latest: "v1.0.0" });
    const r = await runUpdate(h.d);
    expect(r.updated).toBe(false);
    expect(h.calls.replaced).toBeNull();
  });
  test("newer release → downloads to .new then replaces self", async () => {
    const h = makeDeps({ current: "1.0.0", latest: "v1.1.0" });
    const r = await runUpdate(h.d);
    expect(r.updated).toBe(true);
    expect(r.to).toBe("v1.1.0");
    expect(h.calls.downloaded).toBe("/tmp/roost.new");
    expect(h.calls.replaced).toBe("/tmp/roost.new");
  });
});

const originalFetch = globalThis.fetch;
const workdirs: string[] = [];

const originalReleaseBase = process.env.ROOST_RELEASE_BASE_URL;

afterEach(() => {
  globalThis.fetch = originalFetch;
  if (originalReleaseBase === undefined) delete process.env.ROOST_RELEASE_BASE_URL;
  else process.env.ROOST_RELEASE_BASE_URL = originalReleaseBase;
  for (const dir of workdirs.splice(0)) rmSync(dir, { recursive: true, force: true });
});

function mockRelease(binary: string, checksum: string | Response): string[] {
  const requests: string[] = [];
  globalThis.fetch = (async (input: string | URL | Request) => {
    const url = String(input);
    requests.push(url);
    if (url.endsWith(".sha256")) {
      return checksum instanceof Response ? checksum : new Response(checksum, { status: 200 });
    }
    return new Response(binary, { status: 200 });
  }) as typeof fetch;
  return requests;
}

function tempPath(name: string): string {
  const dir = mkdtempSync(join(tmpdir(), "roost-update-"));
  workdirs.push(dir);
  return join(dir, name);
}

describe("fetchAndVerifyReleaseAsset", () => {
  test("streams a matching binary and makes it executable", async () => {
    const binary = "verified release payload";
    const digest = createHash("sha256").update(binary).digest("hex");
    const requests = mockRelease(binary, `${digest}\n`);
    const dest = tempPath("roost.new");

    await fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest });

    expect(readFileSync(dest)).toEqual(Buffer.from(binary));
    expect(statSync(dest).mode & 0o777).toBe(0o755);
    expect(requests).toEqual([
      "https://github.com/cefege/roost/releases/latest/download/roost-linux-x64.sha256",
      "https://github.com/cefege/roost/releases/latest/download/roost-linux-x64",
    ]);
  });

  // The self-updater used to hardcode the GitHub origin while the fleet deploy
  // paths honoured ROOST_RELEASE_BASE_URL, so `roost update` could not be
  // pointed at a mirror. One resolver now serves both.
  test("ROOST_RELEASE_BASE_URL redirects the self-update download to a mirror", async () => {
    const binary = "mirrored release payload";
    const digest = createHash("sha256").update(binary).digest("hex");
    const requests = mockRelease(binary, `${digest}\n`);
    process.env.ROOST_RELEASE_BASE_URL = "https://mirror.internal/roost";
    const dest = tempPath("roost.new");

    await fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest });

    expect(readFileSync(dest)).toEqual(Buffer.from(binary));
    expect(requests).toEqual([
      "https://mirror.internal/roost/roost-linux-x64.sha256",
      "https://mirror.internal/roost/roost-linux-x64",
    ]);
  });

  test("a mirror also serves the tag-pinned Windows manifest origin", async () => {
    const manifest = '{"schemaVersion":1}';
    const digest = createHash("sha256").update(manifest).digest("hex");
    const requests = mockRelease(manifest, `${digest}\n`);

    const pinned = await fetchAndVerifyReleaseAsset("roost-windows-x64.manifest.json", {
      tag: "v9.9.9",
    });
    expect(pinned.url).toBe(
      "https://github.com/cefege/roost/releases/download/v9.9.9/roost-windows-x64.manifest.json",
    );
    expect(pinned.sha256).toBe(digest);
    expect(Buffer.from(pinned.bytes).toString("utf8")).toBe(manifest);

    process.env.ROOST_RELEASE_BASE_URL = "https://mirror.internal/roost";
    const mirrored = await fetchAndVerifyReleaseAsset("roost-windows-x64.manifest.json", {
      tag: "v9.9.9",
    });
    expect(mirrored.url).toBe("https://mirror.internal/roost/roost-windows-x64.manifest.json");
    expect(requests).toHaveLength(4);
  });

  for (const [name, checksum] of [
    ["malformed digest", "not-a-digest\n"],
    ["sidecar that is an error page, not a digest", "<html>404</html>\n"],
    ["altered or truncated binary", "0".repeat(64)],
  ] as const) {
    test(`${name} fails without leaving a candidate`, async () => {
      const dest = tempPath("roost.new");
      writeFileSync(dest, "stale candidate");
      mockRelease("truncated", checksum);

      await expect(fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest }))
        .rejects.toThrow();
      expect(existsSync(dest)).toBe(false);
    });
  }

  // The sidecar is required, but its FORMAT is not a security property — the
  // digest is compared byte-for-byte regardless. A mirror that regenerated its
  // sidecars with plain `sha256sum` publishes "<hash>  <filename>", and hex
  // case is arbitrary. Rejecting either would make ROOST_RELEASE_BASE_URL
  // useless against real mirrors, so both must keep verifying. Do not narrow
  // the parse to make this test fail.
  test("mirror-generated sidecar formats still verify", async () => {
    const binary = "verified release payload";
    const digest = createHash("sha256").update(binary).digest("hex");
    for (const sidecar of [
      `${digest}  roost-linux-x64\n`,
      `${digest} *roost-linux-x64\n`,
      `${digest.toUpperCase()}\n`,
    ]) {
      mockRelease(binary, sidecar);
      const dest = tempPath("roost.new");

      const verified = await fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest });

      expect(verified.sha256).toBe(digest);
      expect(readFileSync(dest)).toEqual(Buffer.from(binary));
    }
  });

  test("a missing checksum fails before downloading the binary", async () => {
    const requests = mockRelease("binary", new Response("missing", { status: 404 }));
    const dest = tempPath("roost.new");

    await expect(fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest }))
      .rejects.toThrow(/checksum download failed: HTTP 404/);
    expect(requests).toHaveLength(1);
    expect(existsSync(dest)).toBe(false);
  });

  test("verification failure leaves the running executable byte-for-byte unchanged", async () => {
    const execPath = tempPath("roost");
    writeFileSync(execPath, "existing executable");
    mockRelease("bad candidate", "0".repeat(64));
    const deps: UpdateDeps = {
      currentVersion: "1.0.0",
      execPath,
      fetchLatestTag: async () => "v1.1.0",
      downloadBinary: async (dest) => {
        await fetchAndVerifyReleaseAsset("roost-linux-x64", { destPath: dest });
      },
      replaceSelf: (candidate) => renameSync(candidate, execPath),
      log: () => {},
    };

    await expect(runUpdate(deps)).rejects.toThrow(/checksum mismatch/);
    expect(readFileSync(execPath, "utf8")).toBe("existing executable");
    expect(existsSync(`${execPath}.new`)).toBe(false);
  });
});

test("source-mode update refuses to replace Bun", async () => {
  expect(basename(process.execPath)).toBe("bun");
  await expect(update([])).rejects.toThrow(/install-binary\.sh/);
});
