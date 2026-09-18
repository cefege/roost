// Pins the worker row's update position: the short SHA and the one shared
// update label, including that a machine merely behind the fleet still prints a
// ✓ and keeps the install healthy — a sleeping laptop is deferred, not broken.
import { describe, expect, test } from "bun:test";
import { statusReportIsHealthy } from "../src/status.ts";
import type { WorkerStatus } from "../src/status-types.ts";
import { renderedStatusLines, statusReportFixture } from "./status-render-fixture.ts";

const COORD_SHA = "b1d1836a9f4c2e1d7a0b5c6d8e9f0a1b2c3d4e5f";

function workerFixture(overrides: Partial<WorkerStatus> = {}): WorkerStatus {
  return {
    fingerprint: "mike-m5-air-fp",
    label: "mike-m5-air",
    os: "darwin",
    reachableAddr: "100.64.0.7",
    gitSha: COORD_SHA,
    keeperRuntime: null,
    terminalCoreCapacity: null,
    coordinatorOpenSessionIds: [],
    lastSeenMs: Date.now(),
    ageMs: 10_000,
    stale: false,
    ...overrides,
  };
}

function workerRow(worker: WorkerStatus): string {
  const lines = renderedStatusLines(statusReportFixture({
    coord: { reachable: true, gitSha: COORD_SHA },
    workers: [worker],
  }));
  return lines.find((line) => line.includes("mike-m5-air")) ?? "";
}

describe("status worker update position", () => {
  test("a worker on the coordinator's SHA is up to date", () => {
    expect(workerRow(workerFixture())).toBe(
      "    ✓ mike-m5-air — last seen 10s ago · b1d1836a · Up to date",
    );
  });

  test("a reachable worker behind the coordinator has an update available", () => {
    expect(workerRow(workerFixture({ gitSha: "0fa77c31de0e4b5a6c7d8e9f0a1b2c3d4e5f6071" }))).toBe(
      "    ✓ mike-m5-air — last seen 10s ago · 0fa77c31 · Update available",
    );
  });

  test("a stale worker behind the coordinator is deferred, not a health failure", () => {
    const deferred = workerFixture({
      label: "m1-us",
      gitSha: "0fa77c31de0e4b5a6c7d8e9f0a1b2c3d4e5f6071",
      ageMs: 3_600_000,
      stale: true,
    });
    const report = statusReportFixture({
      coord: { reachable: true, gitSha: COORD_SHA },
      workers: [deferred],
    });
    const lines = renderedStatusLines(report);

    expect(lines).toContain(
      "    ✗ m1-us — last seen 3600s ago (STALE) · 0fa77c31 · Update pending — offline",
    );
    expect(statusReportIsHealthy(report)).toBe(true);
  });

  test("a worker that reported no SHA prints the unknown label and no SHA", () => {
    const row = workerRow(workerFixture({ gitSha: null }));

    expect(row).toBe("    ✓ mike-m5-air — last seen 10s ago · Version unknown");
  });

  test("an unreachable coordinator cannot classify any worker", () => {
    const lines = renderedStatusLines(statusReportFixture({
      coord: { reachable: false, gitSha: null },
      workers: [workerFixture()],
    }));

    expect(lines).toContain(
      "    ✓ mike-m5-air — last seen 10s ago · b1d1836a · Version unknown",
    );
  });
});
