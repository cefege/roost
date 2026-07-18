// Unit-covers the statusCheckRollup → checks reduction in pr-status.ts. The
// gh subprocess itself isn't driven here (that's the humanchrome real-flow
// verify); this pins the rollup classification that decides ✓/✕/• on the badge.

import { test, expect } from "bun:test";
import { prStatusEq, rollupChecks, type PrStatus } from "../src/pr-status.ts";
import { portsEq } from "../src/listening-ports.ts";

test("rollupChecks — empty/undefined → none", () => {
  expect(rollupChecks(undefined)).toBe("none");
  expect(rollupChecks([])).toBe("none");
});

test("rollupChecks — all completed success → passing", () => {
  expect(rollupChecks([
    { status: "COMPLETED", conclusion: "SUCCESS" },
    { state: "SUCCESS" },
  ])).toBe("passing");
});

test("rollupChecks — any failure/error/cancel → failing (wins over pending)", () => {
  expect(rollupChecks([
    { status: "IN_PROGRESS" },
    { status: "COMPLETED", conclusion: "FAILURE" },
  ])).toBe("failing");
  expect(rollupChecks([{ state: "ERROR" }])).toBe("failing");
  expect(rollupChecks([{ status: "COMPLETED", conclusion: "CANCELLED" }])).toBe("failing");
});

test("rollupChecks — queued/in-progress/pending (no failure) → pending", () => {
  expect(rollupChecks([{ status: "QUEUED" }])).toBe("pending");
  expect(rollupChecks([{ status: "IN_PROGRESS" }])).toBe("pending");
  expect(rollupChecks([{ state: "PENDING" }])).toBe("pending");
  expect(rollupChecks([
    { status: "COMPLETED", conclusion: "SUCCESS" },
    { status: "IN_PROGRESS" },
  ])).toBe("pending");
});

const base: PrStatus = { number: 1, state: "open", checks: "passing", url: "u" };

test("prStatusEq — identical is equal", () => {
  expect(prStatusEq(base, { ...base })).toBe(true);
});

test("prStatusEq — null vs null equal, null vs value not", () => {
  expect(prStatusEq(null, null)).toBe(true);
  expect(prStatusEq(null, base)).toBe(false);
  expect(prStatusEq(base, undefined)).toBe(false);
});

test("prStatusEq — any field diff is not equal", () => {
  expect(prStatusEq(base, { ...base, checks: "failing" })).toBe(false);
  expect(prStatusEq(base, { ...base, state: "merged" })).toBe(false);
  expect(prStatusEq(base, { ...base, number: 2 })).toBe(false);
  expect(prStatusEq(base, { ...base, url: "v" })).toBe(false);
});

test("portsEq — same array equal, order-sensitive/length diff not", () => {
  expect(portsEq([5174, 8765], [5174, 8765])).toBe(true);
  expect(portsEq([], [])).toBe(true);
  expect(portsEq(undefined, [])).toBe(true);
  expect(portsEq([5174], [5174, 8765])).toBe(false);
  expect(portsEq([5174, 8765], [8765, 5174])).toBe(false); // caller pre-sorts
});
