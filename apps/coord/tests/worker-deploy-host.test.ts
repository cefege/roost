import { describe, expect, test } from "bun:test";
import { resolveWorkerDeployTarget, workerDeployHost } from "../src/connect/handlers-workers.ts";

describe("worker deployment address", () => {
  test("uses the live reachable address instead of a non-resolvable label", () => {
    expect(workerDeployHost({
      label: "mike-m1-air-old",
      reachable_addr: "mihai-m1-old.tailnet.ts.net",
    }, "mike-m1-air-old")).toBe("mihai-m1-old.tailnet.ts.net");
  });

  test("falls back to the registered label when no reachable address exists", () => {
    expect(workerDeployHost({ label: "linux-worker", reachable_addr: null }, "worker-fingerprint"))
      .toBe("linux-worker");
  });

  test("uses the authenticated fingerprint for Windows instead of a display label", () => {
    expect(workerDeployHost({
      fp: "a".repeat(64),
      os: "win32",
      label: "Build PC",
      reachable_addr: null,
    }, "Build PC")).toBe("a".repeat(64));
  });

  test("keeps an explicit unregistered host", () => {
    expect(workerDeployHost(undefined, "one-off.tailnet.ts.net")).toBe("one-off.tailnet.ts.net");
  });

  test("rejects duplicate registered labels and reachable names", () => {
    const workers = [
      {
        fp: "a".repeat(64),
        os: "win32",
        label: "Shared worker",
        reachable_addr: "shared.tail.example",
      },
      {
        fp: "b".repeat(64),
        os: "linux",
        label: "Shared worker",
        reachable_addr: "shared.tail.example",
      },
    ];

    expect(resolveWorkerDeployTarget(workers, "Shared worker")).toEqual({
      worker: undefined,
      error: 'ambiguous deploy target "Shared worker" matches multiple registered workers; use the worker fingerprint',
    });
    expect(resolveWorkerDeployTarget(workers, "shared.tail.example")).toEqual({
      worker: undefined,
      error: 'ambiguous deploy target "shared.tail.example" matches multiple registered workers; use the worker fingerprint',
    });
  });

  test("an authenticated fingerprint takes precedence over a colliding display label", () => {
    const fingerprint = "a".repeat(64);
    const authenticated = {
      fp: fingerprint,
      os: "win32",
      label: "Build PC",
      reachable_addr: "build-pc.tail.example",
    };
    const resolution = resolveWorkerDeployTarget([
      authenticated,
      {
        fp: "b".repeat(64),
        os: "win32",
        label: fingerprint,
        reachable_addr: "other.tail.example",
      },
    ], fingerprint);

    expect(resolution).toEqual({ worker: authenticated, error: null });
    expect(workerDeployHost(resolution.worker, fingerprint)).toBe(fingerprint);
  });
});
