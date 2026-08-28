import { clone, create } from "@bufbuild/protobuf";
import { expect } from "bun:test";
import {
  PbCellGridChunkSchema,
  PbCellGridFrameSchema,
  PbCellRowSchema,
  PbCellSpanSchema,
  type PbCellGridChunk,
  type PbCellGridFrame,
  type PbCellRow,
} from "@roost/shared/proto/cell_pb";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import {
  TerminalScreenHub,
  type TerminalScreenHubOptions,
  type TerminalScreenSocketSink,
} from "../src/connect/terminal-screen-hub.ts";

export const SESSION = "40000000-0000-4000-8000-000000000001";
export const STREAM = "50000000-0000-4000-8000-000000000001";
export const OTHER_STREAM = "50000000-0000-4000-8000-000000000002";
export const SNAPSHOT_A = "60000000-0000-4000-8000-000000000001";
export const SNAPSHOT_B = "60000000-0000-4000-8000-000000000002";
export const EPOCH = "grid-epoch-a";

export class TestSink implements TerminalScreenSocketSink {
  readonly events: string[] = [];
  readonly begins: Array<[sessionId: string, streamId: string]> = [];
  readonly states: Array<{ frame: FirehoseFrame; sessionId: string }> = [];
  readonly snapshots: Array<{
    sessionId: string;
    streamId: string;
    frames: readonly FirehoseFrame[];
  }> = [];
  readonly deltas: Array<{ sessionId: string; streamId: string; frame: FirehoseFrame }> = [];
  readonly drops: string[] = [];
  private readonly lanes = new Map<string, string>();

  constructor(private readonly acceptDelta = true) {}

  beginTerminalStream(sessionId: string, streamId: string): boolean {
    if (this.lanes.get(sessionId) === streamId) return false;
    this.lanes.set(sessionId, streamId);
    this.events.push(`begin:${streamId}`);
    this.begins.push([sessionId, streamId]);
    return true;
  }

  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void {
    this.events.push("state");
    this.states.push({ frame, sessionId });
  }

  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): void {
    this.events.push(`snapshot:${streamId}`);
    this.snapshots.push({ sessionId, streamId, frames });
  }

  enqueueTerminalDelta(sessionId: string, streamId: string, frame: FirehoseFrame): boolean {
    this.events.push(`delta:${streamId}`);
    this.deltas.push({ sessionId, streamId, frame });
    return this.acceptDelta;
  }

  dropTerminalSession(sessionId: string): void {
    this.lanes.delete(sessionId);
    this.events.push(`drop:${sessionId}`);
    this.drops.push(sessionId);
  }
}

export function row(index: number, text: string): PbCellRow {
  return create(PbCellRowSchema, {
    index,
    spans: text.length === 0
      ? []
      : [create(PbCellSpanSchema, {
        text,
        columns: 1,
        fg: 256,
        bg: 256,
      })],
  });
}

interface FullOptions {
  streamId?: string;
  epoch?: string;
  seq?: bigint;
  cols?: number;
  rows?: number;
  texts?: string[];
  cursorRow?: number;
  cursorCol?: number;
  cursorVisible?: boolean;
  cursorKeysApp?: boolean;
  bracketedPaste?: boolean;
  mouseTracking?: number;
  mouseSgr?: boolean;
  focusEvents?: boolean;
}

export function fullFrame(options: FullOptions = {}): PbCellGridFrame {
  const rows = options.rows ?? 2;
  const texts = options.texts ?? Array.from({ length: rows }, (_, index) => `r${index}`);
  return create(PbCellGridFrameSchema, {
    sessionId: "worker-owned",
    streamId: options.streamId ?? STREAM,
    gridEpoch: options.epoch ?? EPOCH,
    cols: options.cols ?? 8,
    rows,
    full: true,
    seq: options.seq ?? 1n,
    baseSeq: 0n,
    viewportRows: Array.from({ length: rows }, (_, index) => row(index, texts[index] ?? "")),
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0n,
    sbBase: 0n,
    cursorRow: options.cursorRow ?? 0,
    cursorCol: options.cursorCol ?? 0,
    cursorVisible: options.cursorVisible ?? true,
    cursorKeysApp: options.cursorKeysApp ?? false,
    bracketedPaste: options.bracketedPaste ?? false,
    mouseTracking: options.mouseTracking ?? 0,
    mouseSgr: options.mouseSgr ?? false,
    focusEvents: options.focusEvents ?? false,
  });
}

interface DeltaOptions {
  streamId?: string;
  epoch?: string;
  baseSeq?: bigint;
  seq?: bigint;
  cols?: number;
  rows?: number;
  patchIndex?: number;
  text?: string;
  cursorRow?: number;
  cursorCol?: number;
  cursorVisible?: boolean;
  cursorKeysApp?: boolean;
  bracketedPaste?: boolean;
  mouseTracking?: number;
  mouseSgr?: boolean;
  focusEvents?: boolean;
}

export function deltaFrame(options: DeltaOptions = {}): PbCellGridFrame {
  const baseSeq = options.baseSeq ?? 1n;
  return create(PbCellGridFrameSchema, {
    sessionId: "worker-owned",
    streamId: options.streamId ?? STREAM,
    gridEpoch: options.epoch ?? EPOCH,
    cols: options.cols ?? 8,
    rows: options.rows ?? 2,
    full: false,
    seq: options.seq ?? baseSeq + 1n,
    baseSeq,
    viewportRows: [row(options.patchIndex ?? 1, options.text ?? "changed")],
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0n,
    sbBase: 0n,
    cursorRow: options.cursorRow ?? 1,
    cursorCol: options.cursorCol ?? 2,
    cursorVisible: options.cursorVisible ?? false,
    cursorKeysApp: options.cursorKeysApp ?? true,
    bracketedPaste: options.bracketedPaste ?? true,
    mouseTracking: options.mouseTracking ?? 1000,
    mouseSgr: options.mouseSgr ?? true,
    focusEvents: options.focusEvents ?? true,
  });
}

export function chunks(
  source: PbCellGridFrame,
  groups: readonly (readonly PbCellRow[])[],
  snapshotId = SNAPSHOT_A,
): PbCellGridChunk[] {
  return groups.map((group, chunkIndex) => {
    const part = clone(PbCellGridFrameSchema, source);
    part.viewportRows = group.map((value) => clone(PbCellRowSchema, value));
    return create(PbCellGridChunkSchema, {
      snapshotId,
      chunkIndex,
      chunkCount: groups.length,
      part,
    });
  });
}

function cellFrame(frame: FirehoseFrame): PbCellGridFrame {
  if (frame.frame.case !== "cellGrid") {
    throw new Error(`expected cell grid frame, got ${frame.frame.case}`);
  }
  return frame.frame.value;
}

export function seededFrame(
  sink: TestSink,
  snapshotIndex = sink.snapshots.length - 1,
): PbCellGridFrame {
  const snapshot = sink.snapshots[snapshotIndex];
  if (!snapshot) throw new Error("expected terminal snapshot");
  expect(snapshot.frames).toHaveLength(1);
  return cellFrame(snapshot.frames[0]!);
}

export function texts(frame: PbCellGridFrame): string[] {
  return frame.viewportRows.map((value) => value.spans.map((span) => span.text).join(""));
}

interface TimerEntry {
  callback: () => void;
  delayMs: number;
}

export function makeHarness(clock = { value: 0 }) {
  const requests: Array<[sessionId: string, streamId: string]> = [];
  const unavailable: Array<[sessionId: string, reason: string]> = [];
  const timers = new Map<number, TimerEntry>();
  let nextTimer = 1;
  const options: TerminalScreenHubOptions = {
    requestSnapshot: (sessionId, streamId) => requests.push([sessionId, streamId]),
    unavailable: (sessionId, reason) => unavailable.push([sessionId, reason]),
    now: () => clock.value,
    setTimer: (callback, delayMs) => {
      const timer = nextTimer++;
      timers.set(timer, { callback, delayMs });
      return timer as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer: (timer) => {
      timers.delete(timer as unknown as number);
    },
  };
  const hub = new TerminalScreenHub(options);
  const fireTimer = (timer: number): void => {
    const entry = timers.get(timer);
    if (!entry) throw new Error(`timer ${timer} is not armed`);
    timers.delete(timer);
    entry.callback();
  };
  return { hub, clock, requests, unavailable, timers, fireTimer };
}

export function watch(hub: TerminalScreenHub, sink: TestSink, socketId = "socket-a"): void {
  hub.registerSocket(socketId, sink);
  hub.setWatching(socketId, SESSION, true);
}
