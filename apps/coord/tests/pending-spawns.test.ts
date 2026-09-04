// Covers global session UUID reservation and dashboard-bound reconciliation.
// The registry must reject cross-dashboard collisions before worker dispatch
// while allowing exact retries to share one durable-open result.

import { afterEach, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  pendingSpawnStats,
  rejectPendingSpawn,
  reservePendingSpawn,
  resetPendingSpawnsForTest,
  resolvePendingSpawnOpened,
  type PendingSpawnSignature,
} from "../src/connect/pending-spawns.ts";

const SID = "00000000-0000-4000-8000-000000000616";
const signature: PendingSpawnSignature = {
  callerKey: "browser:tab-a",
  workerFp: "aa".repeat(32),
  dashboardId: "pending-spawns-dashboard",
  kind: "shell",
  folder: "/work",
  cols: 100,
  rows: 31,
};

afterEach(resetPendingSpawnsForTest);

test("exact caller and parameters join one caller-minted spawn", async () => {
  const first = reservePendingSpawn(SID, signature);
  const joined = reservePendingSpawn(SID, { ...signature });
  expect(first.kind).toBe("new");
  expect(joined.kind).toBe("joined");
  if (first.kind === "conflict" || first.kind === "capacity") throw new Error("reservation failed");
  if (joined.kind === "conflict" || joined.kind === "capacity") throw new Error("join failed");
  expect(joined.promise).toBe(first.promise);

  expect(resolvePendingSpawnOpened(
    signature.dashboardId,
    signature.workerFp,
    SID,
    17,
  )).toBe(true);
  // Durable opened is recorded, but normal success still waits for rpc-ok so
  // the worker's first full remains ahead of the HTTP response.
  expect(pendingSpawnStats().pending).toBe(1);
  rejectPendingSpawn(
    signature.dashboardId,
    SID,
    new ConnectError("rpc reply lost", Code.Unavailable),
    false,
  );
  await expect(first.promise).resolves.toEqual({
    sessionId: SID,
    channelId: 17,
  });
  expect(pendingSpawnStats()).toEqual({ pending: 0, resolved: 1 });
});

test("conflicting caller or parameters fail before sharing the pending result", () => {
  reservePendingSpawn(SID, signature);
  expect(reservePendingSpawn(SID, {
    ...signature,
    callerKey: "browser:tab-b",
  }).kind).toBe("conflict");
  expect(reservePendingSpawn(SID, {
    ...signature,
    cols: 101,
  }).kind).toBe("conflict");
});

test("one global session UUID cannot dispatch into two dashboards", () => {
  reservePendingSpawn(SID, signature);
  expect(reservePendingSpawn(SID, {
    ...signature,
    callerKey: "browser-b:tab-a",
    workerFp: "bb".repeat(32),
    dashboardId: "another-dashboard",
  }).kind).toBe("conflict");
});

test("ambiguous failures reconcile with durable opened while definite failures reject", async () => {
  const ambiguousReservation = reservePendingSpawn(SID, signature);
  if (ambiguousReservation.kind === "conflict" || ambiguousReservation.kind === "capacity") {
    throw new Error("reservation failed");
  }

  const ambiguous = new ConnectError("worker disconnected", Code.Unavailable);
  expect(rejectPendingSpawn(
    signature.dashboardId,
    SID,
    ambiguous,
    false,
  )).toBe(true);
  expect(pendingSpawnStats().pending).toBe(1);
  expect(resolvePendingSpawnOpened(
    signature.dashboardId,
    signature.workerFp,
    SID,
    23,
  )).toBe(true);
  await expect(ambiguousReservation.promise).resolves.toEqual({
    sessionId: SID,
    channelId: 23,
  });

  const definiteSession = "00000000-0000-4000-8000-000000000617";
  const definiteReservation = reservePendingSpawn(definiteSession, signature);
  if (definiteReservation.kind === "conflict" || definiteReservation.kind === "capacity") {
    throw new Error("reservation failed");
  }
  const definite = new ConnectError("keeper rejected spawn", Code.Internal);
  expect(rejectPendingSpawn(
    signature.dashboardId,
    definiteSession,
    definite,
    true,
  )).toBe(true);
  await expect(definiteReservation.promise).rejects.toThrow("keeper rejected spawn");
});
