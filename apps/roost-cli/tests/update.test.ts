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
  downloadVerifiedReleaseAsset,
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
  test("unsupported platform throws rather than guessing", () => {
    expect(() => releaseAssetName("win32", "x64")).toThrow(/no prebuilt roost binary/);
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

afterEach(() => {
  globalThis.fetch = originalFetch;
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

describe("downloadVerifiedReleaseAsset", () => {
  test("streams a matching binary and makes it executable", async () => {
    const binary = "verified release payload";
    const digest = createHash("sha256").update(binary).digest("hex");
    const requests = mockRelease(binary, `${digest}\n`);
    const dest = tempPath("roost.new");

    await downloadVerifiedReleaseAsset("roost-linux-x64", dest);

    expect(readFileSync(dest)).toEqual(Buffer.from(binary));
    expect(statSync(dest).mode & 0o777).toBe(0o755);
    expect(requests.map((url) => basename(url))).toEqual([
      "roost-linux-x64.sha256",
      "roost-linux-x64",
    ]);
  });

  for (const [name, checksum] of [
    ["malformed digest", "not-a-digest\n"],
    ["uppercase digest", "A".repeat(64)],
    ["altered or truncated binary", "0".repeat(64)],
  ] as const) {
    test(`${name} fails without leaving a candidate`, async () => {
      const dest = tempPath("roost.new");
      writeFileSync(dest, "stale candidate");
      mockRelease("truncated", checksum);

      await expect(downloadVerifiedReleaseAsset("roost-linux-x64", dest)).rejects.toThrow();
      expect(existsSync(dest)).toBe(false);
    });
  }

  test("a missing checksum fails before downloading the binary", async () => {
    const requests = mockRelease("binary", new Response("missing", { status: 404 }));
    const dest = tempPath("roost.new");

    await expect(downloadVerifiedReleaseAsset("roost-linux-x64", dest)).rejects.toThrow(
      /checksum download failed: HTTP 404/,
    );
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
      downloadBinary: (dest) => downloadVerifiedReleaseAsset("roost-linux-x64", dest),
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
