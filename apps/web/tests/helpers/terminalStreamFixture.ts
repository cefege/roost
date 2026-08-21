import { afterEach, beforeAll, beforeEach, mock, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CELL_GRID_CHUNK_STALL_MS,
  chunkCellGridFrame,
  type CellGridFrame,
  type CellRow,
} from "@roost/shared/cell";
import { cellFrameToProto } from "@roost/shared/cell/cell-proto";
import {
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
} from "@roost/shared/proto/sync_pb";
import type { CellGridRenderer } from "../../src/lib/cellRenderer.ts";

import type * as TerminalStreamModule from "../../src/store/terminal-stream.ts";
import type { TerminalViewHandleStatus } from "../../src/store/terminal-stream-types.ts";
interface TestSyncState {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
  ready: boolean;
}
interface TestCommand {
  case: string;
  value: Record<string, unknown>;
}

let syncState: TestSyncState | null = {
  socketGeneration: 1,
  socketId: "socket-1",
  processEpoch: "process-1",
  domainGeneration: 11n,
  ready: true,
};
let generationHandler: ((state: TestSyncState | null) => void) | null = null;
const sent: TestCommand[] = [];

mock.module("../../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => syncState,
  sendSyncV2Command: (value: TestCommand) => {
    sent.push(value);
    return syncState?.ready === true;
  },
  registerSyncV2GenerationHandler: (
    handler: (state: TestSyncState | null) => void,
  ) => {
    generationHandler = handler;
    handler(syncState);
    return () => {
      if (generationHandler === handler) generationHandler = null;
    };
  },
}));

mock.module("../../src/lib/diag.ts", () => ({
  markPhaseOnce: () => undefined,
  recordCellLag: () => undefined,
}));

// The transport mock must be installed before this singleton registers its
// generation callback; defer loading until Bun has finished evaluating this
// fixture so the reloaded module cannot observe this fixture's export TDZ.
let loadedTerminalStream: typeof TerminalStreamModule | null = null;
const terminalStream = new Proxy({} as typeof TerminalStreamModule, {
  get: (_target, property) => {
    if (!loadedTerminalStream) throw new Error("terminal stream fixture is not initialized");
    return Reflect.get(loadedTerminalStream, property);
  },
});
beforeAll(async () => {
  loadedTerminalStream = await import("../../src/store/terminal-stream.ts");
  await Promise.resolve();
});

const SESSION_ID = "session-browser-replica";
const STREAM_A = "10000000-0000-4000-8000-000000000001";
const STREAM_B = "10000000-0000-4000-8000-000000000002";
const SNAPSHOT_A = "20000000-0000-4000-8000-000000000001";
const EPOCH_A = "grid-epoch-a";

function row(index: number, text: string, linkUri?: string): CellRow {
  return {
    index,
    spans: text.length === 0
      ? []
      : [{
          text,
          columns: text.length,
          fg: 256,
          bg: 256,
          flags: 0,
          ...(linkUri
            ? { linkKey: `link-${index}`, linkUri }
            : {}),
        }],
  };
}

function full(
  streamId = STREAM_A,
  viewport: CellRow[] = [row(0, "A")],
  seq = 1,
): CellGridFrame {
  return {
    streamId,
    gridEpoch: EPOCH_A,
    cols: Math.max(1, ...viewport.map((value) => value.spans.reduce(
      (total, span) => total + span.columns,
      0,
    ))),
    rows: viewport.length,
    full: true,
    viewportRows: viewport,
    scrollbackRows: [],
    scrollbackAppend: [],
    scrollbackTotal: 0,
    sbBase: 0,
    baseSeq: 0,
    seq,
    cursorRow: 0,
    cursorCol: 0,
    cursorVisible: true,
    altScreen: false,
    cursorKeysApp: false,
    bracketedPaste: false,
    mouseTracking: 0,
    mouseSgr: false,
    focusEvents: false,
  };
}

function delta(
  seq: number,
  text: string,
  streamId = STREAM_A,
  baseSeq = seq - 1,
): CellGridFrame {
  return {
    ...full(streamId),
    full: false,
    viewportRows: [row(0, text)],
    baseSeq,
    seq,
  };
}

function viewCommands(): TestCommand[] {
  return sent.filter((value) => value.case === "terminalView");
}

function resyncCommands(): TestCommand[] {
  return sent.filter((value) => value.case === "terminalResync");
}

function latestViewCommand(): TestCommand {
  const value = viewCommands().at(-1);
  if (!value) throw new Error("test did not publish a terminal view command");
  return value;
}

function acceptView(
  viewId: string,
  revision: bigint,
  streamId = STREAM_A,
  cols = 1,
  rows = 1,
): void {
  terminalStream.dispatchTerminalViewState(create(TerminalViewStateFrameSchema, {
    viewId,
    sessionId: SESSION_ID,
    revision,
    active: true,
    streamId,
    status: TerminalViewStatus.ACCEPTED,
    effectiveCols: cols,
    effectiveRows: rows,
    reason: "",
  }));
}

function rejectView(
  viewId: string,
  revision: bigint,
  reason = "conflicting intent",
): void {
  terminalStream.dispatchTerminalViewState(create(TerminalViewStateFrameSchema, {
    viewId,
    sessionId: SESSION_ID,
    revision,
    active: true,
    streamId: STREAM_A,
    status: TerminalViewStatus.REJECTED,
    effectiveCols: 1,
    effectiveRows: 1,
    reason,
  }));
}

class RecordingRenderer {
  readonly fullFrames: CellGridFrame[] = [];
  readonly deltaFrames: CellGridFrame[] = [];
  mutateRows = false;

  applyFullFrame(frame: CellGridFrame): boolean {
    this.fullFrames.push(frame);
    if (this.mutateRows && frame.viewportRows[0]) frame.viewportRows[0].index = 99;
    return true;
  }

  applyDeltaFrame(frame: CellGridFrame): boolean {
    this.deltaFrames.push(frame);
    if (this.mutateRows && frame.viewportRows[0]) frame.viewportRows[0].index = 99;
    return true;
  }
}

function renderer(value: RecordingRenderer): CellGridRenderer {
  return value as unknown as CellGridRenderer;
}
function updateSyncState(next: TestSyncState | null): void {
  syncState = next;
  generationHandler?.(syncState);
}


beforeEach(() => {
  vi.useFakeTimers();
  terminalStream._resetTerminalStreamForTest();
  sent.length = 0;
  syncState = {
    socketGeneration: 1,
    socketId: "socket-1",
    processEpoch: "process-1",
    domainGeneration: 11n,
    ready: true,
  };
  generationHandler?.(syncState);
});

afterEach(() => {
  terminalStream._resetTerminalStreamForTest();
  vi.useRealTimers();
});
export {
  CELL_GRID_CHUNK_STALL_MS,
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  STREAM_B,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  delta,
  full,
  latestViewCommand,
  rejectView,
  renderer,
  resyncCommands,
  row,
  terminalStream,
  updateSyncState,
  viewCommands,
};
export type { TerminalViewHandleStatus };

