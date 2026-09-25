// Durable attachment operation tests. They pin the carrier fence and committed
// receipt behavior that resolve lost direct ACKs without replaying bytes through
// coordinator fallback or a second direct socket.

import { afterEach, expect, test, vi } from "bun:test";
import fs from "node:fs";
import path from "node:path";
import { randomUUID } from "node:crypto";
import { probeAttachment, recordAttachmentHash } from "../../src/attachments/attachment-file-store.ts";
import {
  createAttachmentOperation,
  createAttachmentOperationPaths,
  persistAttachmentOperation,
} from "../../src/attachments/attachment-operation-journal.ts";
import { ATTACHMENT_OPERATION_IDLE_MS, AttachmentOperationOwner } from "../../src/attachments/attachment-operation-owner.ts";
import { attachmentSessionDir } from "../../src/attachments/attachment-reaper.ts";

const cleanups: Array<() => void> = [];

afterEach(() => {
  vi.restoreAllMocks();
  for (const cleanup of cleanups.splice(0)) cleanup();
});

function digest(data: Uint8Array): string {
  return new Bun.CryptoHasher("sha256").update(data).digest("hex");
}

function chunk(
  sessionId: string,
  requestId: string,
  data: Uint8Array,
  options: Partial<{
    carrier: "coordinator" | "direct";
    carrierId: string;
    seq: number;
    offset: number;
    last: boolean;
    totalBytes: number;
  }> = {},
) {
  return {
    requestId,
    sessionId,
    filename: "carrier.bin",
    shortPath: false,
    totalBytes: options.totalBytes,
    carrier: options.carrier ?? "direct",
    carrierId: options.carrierId ?? "socket-a",
    seq: options.seq ?? 0,
    offset: options.offset ?? 0,
    data,
    last: options.last ?? false,
    chunkSha256: digest(data),
  };
}

test("rejects coordinator continuation after direct seq 0 and retains only status", async () => {
  const sessionId = `test-carrier-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const firstOwner = new AttachmentOperationOwner();
  const first = await firstOwner.accept(chunk(sessionId, requestId, Uint8Array.of(1), { totalBytes: 2 }));
  expect(first).toMatchObject({ kind: "receipt", receipt: { nextSeq: 1, bytesReceived: 1 } });
  firstOwner.detachDirectCarrier("socket-a");

  const restartedOwner = new AttachmentOperationOwner();
  expect(restartedOwner.status(sessionId, requestId)).toMatchObject({
    nextSeq: 1,
    bytesReceived: 1,
    committed: false,
    lastChunkSha256: digest(Uint8Array.of(1)),
  });
  const fallback = await restartedOwner.accept(chunk(sessionId, requestId, Uint8Array.of(2), {
    carrier: "coordinator",
    carrierId: "",
    seq: 1,
    offset: 1,
    last: true,
    totalBytes: 2,
  }));
  expect(fallback).toEqual({ kind: "error", error: "upload_mismatch" });
  expect(restartedOwner.status(sessionId, requestId)).toMatchObject({
    nextSeq: 1,
    bytesReceived: 1,
    committed: false,
    error: "",
  });
  expect(fs.existsSync(`${attachmentSessionDir(sessionId)}/carrier.bin`)).toBe(false);
});

test("returns the durable final receipt to an exact direct duplicate after restart", async () => {
  const sessionId = `test-receipt-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const data = Uint8Array.of(9, 8, 7);
  const initialOwner = new AttachmentOperationOwner();
  const initial = await initialOwner.accept(chunk(sessionId, requestId, data, { last: true, totalBytes: data.byteLength }));
  if (initial.kind !== "receipt") throw new Error("final operation did not commit");
  expect(initial.receipt.committed).toBe(true);

  const restartedOwner = new AttachmentOperationOwner();
  const duplicate = await restartedOwner.accept(chunk(sessionId, requestId, data, { last: true, totalBytes: data.byteLength }));
  expect(duplicate).toEqual(initial);
  expect(restartedOwner.status(sessionId, requestId)).toMatchObject({
    committed: true,
    absPath: initial.receipt.absPath,
    lastChunkSha256: digest(data),
  });
});

test("never blocks the event loop on fsync after seq 0 and flushes the final commit asynchronously", async () => {
  const directSessionId = `test-direct-sync-${randomUUID()}`;
  const coordinatorSessionId = `test-coordinator-buffer-${randomUUID()}`;
  const directRequestId = randomUUID();
  const coordinatorRequestId = randomUUID();
  cleanups.push(
    () => fs.rmSync(attachmentSessionDir(directSessionId), { recursive: true, force: true }),
    () => fs.rmSync(attachmentSessionDir(coordinatorSessionId), { recursive: true, force: true }),
  );
  const fsync = vi.spyOn(fs, "fsyncSync");
  const directOwner = new AttachmentOperationOwner();
  expect(await directOwner.accept(chunk(directSessionId, directRequestId, Uint8Array.of(1), {
    seq: 0,
    offset: 0,
    totalBytes: 3,
  }))).toMatchObject({ kind: "receipt", receipt: { nextSeq: 1 } });
  fsync.mockClear();
  expect(await directOwner.accept(chunk(directSessionId, directRequestId, Uint8Array.of(2), {
    seq: 1,
    offset: 1,
    totalBytes: 3,
  }))).toMatchObject({ kind: "receipt", receipt: { nextSeq: 2 } });
  expect(fsync).not.toHaveBeenCalled();
  expect(new AttachmentOperationOwner().status(directSessionId, directRequestId)).toMatchObject({
    nextSeq: 2,
    bytesReceived: 2,
  });
  directOwner.detachDirectCarrier("socket-a");

  const coordinatorOwner = new AttachmentOperationOwner();
  expect(await coordinatorOwner.accept(chunk(coordinatorSessionId, coordinatorRequestId, Uint8Array.of(3), {
    carrier: "coordinator",
    carrierId: "",
    seq: 0,
    offset: 0,
    totalBytes: 3,
  }))).toMatchObject({ kind: "receipt", receipt: { nextSeq: 1 } });
  fsync.mockClear();
  expect(await coordinatorOwner.accept(chunk(coordinatorSessionId, coordinatorRequestId, Uint8Array.of(4), {
    carrier: "coordinator",
    carrierId: "",
    seq: 1,
    offset: 1,
    totalBytes: 3,
  }))).toMatchObject({ kind: "receipt", receipt: { nextSeq: 2 } });
  expect(fsync).not.toHaveBeenCalled();
  fsync.mockClear();
  const asyncOpen = vi.spyOn(fs.promises, "open");
  const final = await coordinatorOwner.accept(chunk(coordinatorSessionId, coordinatorRequestId, Uint8Array.of(5), {
    carrier: "coordinator",
    carrierId: "",
    seq: 2,
    offset: 2,
    last: true,
    totalBytes: 3,
  }));
  expect(final).toMatchObject({ kind: "receipt", receipt: { committed: true } });
  expect(fsync).not.toHaveBeenCalled();
  if (final.kind !== "receipt") throw new Error("coordinator final chunk did not commit");
  expect(asyncOpen.mock.calls.map(([target]) => target)).toContain(final.receipt.absPath);
  expect(fs.readFileSync(final.receipt.absPath)).toEqual(Buffer.from([3, 4, 5]));
});

test("rejects a recovered final-name collision without corrupting its manifest", async () => {
  const sessionId = `test-final-name-collision-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const expected = Uint8Array.of(1, 2, 3);
  const occupied = Uint8Array.of(4, 5, 6);
  const created = createAttachmentOperation({
    requestId,
    sessionId,
    filename: "collision.bin",
    shortPath: false,
    totalBytes: expected.byteLength,
  }, "direct", "socket-a");
  if (!created) throw new Error("attachment operation was not created");
  const { paths, journal } = created;
  const destination = path.join(paths.sessionDir, "collision.bin");
  const expectedDigest = digest(expected);
  const occupiedDigest = digest(occupied);
  fs.writeFileSync(paths.tempPath, expected);
  fs.writeFileSync(destination, occupied);
  recordAttachmentHash(paths.sessionDir, occupiedDigest, "collision.bin");
  journal.nextSeq = 1;
  journal.bytesWritten = expected.byteLength;
  journal.lastChunkFinal = true;
  journal.lastChunkSha256 = expectedDigest;
  journal.finalName = "collision.bin";
  journal.contentSha256 = expectedDigest;
  persistAttachmentOperation(paths, journal);

  expect(new AttachmentOperationOwner().status(sessionId, requestId)).toMatchObject({
    committed: false,
    error: "write_failed",
  });
  expect(fs.readFileSync(destination)).toEqual(Buffer.from(occupied));
  expect(probeAttachment(sessionId, occupiedDigest, false).hit).toBe(true);
  expect(probeAttachment(sessionId, expectedDigest, false)).toEqual({ hit: false, abs_path: "" });
});

test("idle sweep fails a silent relay upload and parks a silent direct upload resumably", async () => {
  const sessionId = `test-idle-${randomUUID()}`;
  const relayId = randomUUID();
  const directId = randomUUID();
  const freshId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  let now = 1_000;
  const owner = new AttachmentOperationOwner(() => now);
  const relay = { carrier: "coordinator" as const, carrierId: "", totalBytes: 2 };
  expect(await owner.accept(chunk(sessionId, relayId, Uint8Array.of(1), relay))).toMatchObject({ kind: "receipt" });
  expect(await owner.accept(chunk(sessionId, directId, Uint8Array.of(1), { totalBytes: 2 }))).toMatchObject({ kind: "receipt" });
  now += ATTACHMENT_OPERATION_IDLE_MS;
  expect(await owner.accept(chunk(sessionId, freshId, Uint8Array.of(1), relay))).toMatchObject({ kind: "receipt" });
  now += 1;
  owner.sweepIdle();

  expect(fs.existsSync(createAttachmentOperationPaths(sessionId, relayId)!.tempPath)).toBe(false);
  expect(await owner.accept(chunk(sessionId, relayId, Uint8Array.of(2), { ...relay, seq: 1, offset: 1, last: true })))
    .toEqual({ kind: "error", error: "upload_not_found" });
  expect(owner.status(sessionId, directId)).toMatchObject({ nextSeq: 1, bytesReceived: 1, error: "" });
  expect(await owner.accept(chunk(sessionId, directId, Uint8Array.of(2), { seq: 1, offset: 1, last: true, totalBytes: 2 })))
    .toMatchObject({ kind: "receipt", receipt: { committed: true } });
  expect(await owner.accept(chunk(sessionId, freshId, Uint8Array.of(2), { ...relay, seq: 1, offset: 1, last: true })))
    .toMatchObject({ kind: "receipt", receipt: { committed: true } });
});

test("status of a detached partial upload reads counters without scanning its bytes", async () => {
  const sessionId = `test-status-scan-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const owner = new AttachmentOperationOwner();
  await owner.accept(chunk(sessionId, requestId, new Uint8Array(4096), { totalBytes: 8192 }));
  owner.detachDirectCarrier("socket-a");
  const readSync = vi.spyOn(fs, "readSync");

  expect(new AttachmentOperationOwner().status(sessionId, requestId)).toMatchObject({
    nextSeq: 1,
    bytesReceived: 4096,
    error: "",
  });
  expect(readSync).not.toHaveBeenCalled();
});

test.skipIf(process.platform === "win32")("a failed POSIX directory flush withholds the final receipt", async () => {
  const sessionId = `test-dir-flush-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const operationDir = createAttachmentOperationPaths(sessionId, requestId)!.operationDir;
  const open = fs.promises.open;
  const owner = new AttachmentOperationOwner();
  expect(await owner.accept(chunk(sessionId, requestId, Uint8Array.of(6), { totalBytes: 2 })))
    .toMatchObject({ kind: "receipt", receipt: { committed: false } });
  vi.spyOn(fs.promises, "open").mockImplementation(async (target, flags, mode) => {
    if (target === operationDir) throw Object.assign(new Error("EIO"), { code: "EIO" });
    return open(target, flags, mode);
  });

  expect(await owner.accept(chunk(sessionId, requestId, Uint8Array.of(7), { seq: 1, offset: 1, last: true, totalBytes: 2 })))
    .toEqual({ kind: "error", error: "write_failed" });
});

test("the journal records upload metadata and progress, never chunk bytes", async () => {
  const sessionId = `test-journal-shape-${randomUUID()}`;
  const requestId = randomUUID();
  cleanups.push(() => fs.rmSync(attachmentSessionDir(sessionId), { recursive: true, force: true }));
  const owner = new AttachmentOperationOwner();
  await owner.accept(chunk(sessionId, requestId, new Uint8Array(64 * 1024).fill(7), {
    carrier: "coordinator",
    carrierId: "",
    totalBytes: 128 * 1024,
  }));

  const journalText = fs.readFileSync(createAttachmentOperationPaths(sessionId, requestId)!.journalPath, "utf8");
  expect(journalText.length).toBeLessThan(2_048);
  expect(Object.keys(JSON.parse(journalText) as object)).not.toContain("data");
});
