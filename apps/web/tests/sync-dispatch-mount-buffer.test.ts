import { afterEach, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { asChannelId, asSessionId, asWorkerFp, type Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  _dispatchCell,
  acquireCellMountClaim,
  cellMountBufferStats,
  pruneCellFrameCount,
  registerCellHandler,
  resetCellMountBuffers,
} from "../src/store/sync-dispatch.ts";

const SID = "00000000-0000-4000-8000-000000000515";

function installKnownSession(): void {
  const session: Session = {
    id: asSessionId(SID),
    worker_fp: asWorkerFp("aa".repeat(32)),
    channel: asChannelId(9),
    kind: "shell",
    cwd: "/work",
    spawn_cwd: "/work",
    workspace_id: null,
    status: "open",
    created_at: 1,
    closed_at: null,
    custom_title: null,
  };
  setRootStore("sessions", SID, session);
}

function frame(seq: number, full: boolean) {
  return create(PbCellGridFrameSchema, {
    sessionId: SID,
    seq: BigInt(seq),
    full,
    cols: 80,
    rows: 24,
    gridEpoch: "mount-gap",
  });
}

beforeEach(() => {
  pruneCellFrameCount(SID);
  resetCellMountBuffers();
  for (const id of Object.keys(rootStore.sessions)) {
    setRootStore("sessions", id, undefined as unknown as Session);
  }
});

afterEach(() => {
  pruneCellFrameCount(SID);
  resetCellMountBuffers();
});

test("unknown and unclaimed cells remain live-drop", () => {
  _dispatchCell(frame(1, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
  installKnownSession();
  _dispatchCell(frame(1, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
});

test("known claimed mount gap drains one full followed by ordered deltas synchronously", () => {
  installKnownSession();
  const claim = acquireCellMountClaim(SID);
  claim.activate();
  _dispatchCell(frame(10, true));
  _dispatchCell(frame(11, false));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 1, frames: 2 });

  const seen: Array<{ seq: number; full: boolean }> = [];
  const stop = registerCellHandler(SID, (value) => {
    seen.push({ seq: value.seq, full: value.full });
  });
  expect(seen).toEqual([
    { seq: 10, full: true },
    { seq: 11, full: false },
  ]);
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
  stop();
  claim.release();
});

test("delta-before-full never buffers and requests exactly one current repair on mount", () => {
  installKnownSession();
  const claim = acquireCellMountClaim(SID);
  claim.activate();
  _dispatchCell(frame(2, false));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, pendingRepairs: 1 });
  let repairs = 0;
  const stop = registerCellHandler(SID, () => {}, () => { repairs++; });
  expect(repairs).toBe(1);
  expect(cellMountBufferStats().pendingRepairs).toBe(0);
  stop();
  claim.release();
});

test("latest owner survives stale release, stale handler disposal, and stale double release", () => {
  installKnownSession();
  const ownerA = acquireCellMountClaim(SID);
  ownerA.activate();
  const stopA = registerCellHandler(SID, () => {
    throw new Error("superseded handler received a frame");
  });

  const ownerB = acquireCellMountClaim(SID);
  ownerB.activate();
  const seen: number[] = [];
  const stopB = registerCellHandler(SID, (value) => { seen.push(value.seq); });

  stopA();
  _dispatchCell(frame(1, true));
  expect(seen).toEqual([1]);
  stopB();

  _dispatchCell(frame(2, false));
  _dispatchCell(frame(10, true));
  _dispatchCell(frame(11, false));
  ownerA.release();
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 1,
    frames: 2,
    pendingRepairs: 1,
  });

  let repairs = 0;
  const currentSeen: number[] = [];
  const stopCurrent = registerCellHandler(
    SID,
    (value) => { currentSeen.push(value.seq); },
    () => { repairs++; },
  );
  expect(repairs).toBe(1);
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 0,
    frames: 0,
    pendingRepairs: 0,
  });
  ownerA.release();
  _dispatchCell(frame(12, true));
  expect(currentSeen).toEqual([10, 11, 12]);
  stopCurrent();
  ownerB.release();
});

test("current owner release clears its buffer and repair latch and disables buffering", () => {
  installKnownSession();
  const claim = acquireCellMountClaim(SID);
  claim.activate();
  _dispatchCell(frame(2, false));
  _dispatchCell(frame(10, true));
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 1,
    frames: 1,
    pendingRepairs: 1,
  });

  claim.release();
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 0,
    frames: 0,
    pendingRepairs: 0,
  });
  _dispatchCell(frame(20, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
});

test("current owner can deactivate, clear its latch, and reactivate", () => {
  installKnownSession();
  const claim = acquireCellMountClaim(SID);
  claim.activate();
  _dispatchCell(frame(2, false));
  _dispatchCell(frame(10, true));
  claim.deactivate();
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 0,
    frames: 0,
    pendingRepairs: 0,
  });

  _dispatchCell(frame(20, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
  claim.activate();
  _dispatchCell(frame(30, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 1, frames: 1 });
  claim.release();
});

test("superseded owner cannot deactivate or release its successor state", () => {
  installKnownSession();
  const ownerA = acquireCellMountClaim(SID);
  ownerA.activate();
  const ownerB = acquireCellMountClaim(SID);
  ownerB.activate();
  _dispatchCell(frame(2, false));
  _dispatchCell(frame(10, true));

  ownerA.deactivate();
  ownerA.release();
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 1,
    frames: 1,
    pendingRepairs: 1,
  });
  ownerB.release();
});

test("generation reset clears buffered generation data but current owner can reactivate", () => {
  installKnownSession();
  const resetOwner = acquireCellMountClaim(SID);
  resetOwner.activate();
  _dispatchCell(frame(2, false));
  _dispatchCell(frame(10, true));

  resetCellMountBuffers();
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 0,
    frames: 0,
    pendingRepairs: 0,
  });
  _dispatchCell(frame(20, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
  resetOwner.activate();
  _dispatchCell(frame(30, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 1, frames: 1 });

  const successor = acquireCellMountClaim(SID);
  successor.activate();
  resetOwner.release();
  resetOwner.deactivate();
  _dispatchCell(frame(31, false));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 1, frames: 2 });
  successor.release();
});

test("session close force-clears ownership permanently", () => {
  installKnownSession();
  const closeOwner = acquireCellMountClaim(SID);
  closeOwner.activate();
  _dispatchCell(frame(30, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 1, frames: 1 });
  pruneCellFrameCount(SID);
  expect(cellMountBufferStats()).toMatchObject({
    sessions: 0,
    frames: 0,
    pendingRepairs: 0,
  });
  closeOwner.activate();
  _dispatchCell(frame(40, true));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, frames: 0 });
  closeOwner.release();
});
