import { afterEach, beforeEach, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import { PbCellGridFrameSchema } from "@roost/shared/proto/cell_pb";
import { asChannelId, asSessionId, asWorkerFp, type Session } from "@roost/shared/wire";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  _dispatchCell,
  cellMountBufferStats,
  registerCellHandler,
  resetCellMountBuffers,
  setCellMountClaimActive,
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
  resetCellMountBuffers();
  for (const id of Object.keys(rootStore.sessions)) {
    setRootStore("sessions", id, undefined as unknown as Session);
  }
});

afterEach(() => {
  setCellMountClaimActive(SID, false);
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
  setCellMountClaimActive(SID, true);
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
});

test("delta-before-full never buffers and requests exactly one current repair on mount", () => {
  installKnownSession();
  setCellMountClaimActive(SID, true);
  _dispatchCell(frame(2, false));
  expect(cellMountBufferStats()).toMatchObject({ sessions: 0, pendingRepairs: 1 });
  let repairs = 0;
  const stop = registerCellHandler(SID, () => {}, () => { repairs++; });
  expect(repairs).toBe(1);
  expect(cellMountBufferStats().pendingRepairs).toBe(0);
  stop();
});
