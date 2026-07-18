// roost update contract: version-compare semantics + orchestration (skip when
// no release / up to date, download-then-replace when newer) with injected
// deps — no network, no real binary swap.
import { describe, expect, test } from "bun:test";
import { needsUpdate, runUpdate, type UpdateDeps } from "../src/update.ts";

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
