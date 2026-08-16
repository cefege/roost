import { afterEach, expect, test } from "bun:test";
import { Code, ConnectError } from "@connectrpc/connect";
import {
  configurePendingSpawnPreclaim,
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
  kind: "shell",
  folder: "/work",
  cols: 100,
  rows: 31,
  preclaimInitialViewport: true,
  requestedClientSeq: 7n,
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

  expect(configurePendingSpawnPreclaim(SID, 9n, () => true)).toBe(true);
  expect(resolvePendingSpawnOpened(signature.workerFp, SID, 17)).toBe(true);
  // Durable opened is recorded, but normal success still waits for rpc-ok so
  // the worker's first full remains ahead of the HTTP response.
  expect(pendingSpawnStats().pending).toBe(1);
  rejectPendingSpawn(
    SID,
    new ConnectError("rpc reply lost", Code.Unavailable),
    false,
  );
  await expect(first.promise).resolves.toEqual({
    sessionId: SID,
    channelId: 17,
    initialViewportPreclaimed: true,
    effectiveClientSeq: 9n,
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

test("only definite failure rolls back provisional membership", async () => {
  const reservation = reservePendingSpawn(SID, signature);
  if (reservation.kind === "conflict" || reservation.kind === "capacity") throw new Error("reservation failed");
  let rollbacks = 0;
  configurePendingSpawnPreclaim(SID, 7n, () => { rollbacks++; return true; });

  const ambiguous = new ConnectError("worker disconnected", Code.Unavailable);
  expect(rejectPendingSpawn(SID, ambiguous, false)).toBe(true);
  expect(rollbacks).toBe(0);
  expect(pendingSpawnStats().pending).toBe(1);

  const definite = new ConnectError("keeper rejected spawn", Code.Internal);
  expect(rejectPendingSpawn(SID, definite, true)).toBe(true);
  expect(rollbacks).toBe(1);
  await expect(reservation.promise).rejects.toThrow("keeper rejected spawn");
});
