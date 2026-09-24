import { afterEach, beforeAll, beforeEach, mock, vi } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  CELL_GRID_CHUNK_STALL_MS,
  chunkCellGridFrame,
  cloneCellGridFrame,
  type CellGridFrame,
  type CellRow,
} from "@roost/protocol/cell";
import { cellFrameToProto } from "@roost/protocol/cell/cell-proto";
import {
  TerminalViewStateFrameSchema,
  TerminalViewStatus,
} from "@roost/protocol/proto/sync_pb";
import type { CellGridRenderer } from "../../src/renderer/cellRenderer.ts";

import type * as TerminalStreamModule from "../../src/store/terminal-stream.ts";
import type { TerminalViewHandleStatus } from "../../src/store/terminal-stream-types.ts";
import { terminalDirectRegistry } from "../../src/store/terminal-stream-transport.ts";
type TerminalStreamFixtureModule = Omit<
  typeof TerminalStreamModule,
  "dispatchTerminalCellFrame" | "dispatchTerminalCellChunk" | "dispatchTerminalViewState"
> & {
  dispatchTerminalCellFrame(
    frame: Parameters<typeof TerminalStreamModule.dispatchTerminalCellFrame>[0],
  ): void;
  dispatchTerminalCellChunk(
    frame: Parameters<typeof TerminalStreamModule.dispatchTerminalCellChunk>[0],
  ): void;
  dispatchTerminalViewState(
    frame: Parameters<typeof TerminalStreamModule.dispatchTerminalViewState>[0],
  ): void;
};
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

/** The generation the fixture's Sync socket owns. Suites flip only `ready` to
 *  take the publication target away without rotating the generation, so this
 *  identity has exactly one definition on both sides of that boundary. */
const CURRENT_SYNC_OWNER: Omit<TestSyncState, "ready"> = {
  socketGeneration: 1,
  socketId: "socket-1",
  processEpoch: "process-1",
  domainGeneration: 11n,
};
let syncState: TestSyncState | null = { ...CURRENT_SYNC_OWNER, ready: true };
let generationHandler: ((state: TestSyncState | null) => void) | null = null;
const sent: TestCommand[] = [];
const generationRecoveries: Array<{
  expected: Omit<TestSyncState, "ready">;
  reason: string;
}> = [];
let visible = true;
let focused = true;

mock.module("../../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => syncState,
  registerSyncV2ControlHandler: () => () => undefined,
  registerSyncV2ProbeResultHandler: () => () => undefined,
  sendSyncV2Command: (value: TestCommand) => {
    sent.push(value);
    return syncState?.ready === true;
  },
  requestSyncGenerationRecovery: (
    expected: Omit<TestSyncState, "ready">,
    reason: string,
  ) => {
    if (
      !syncState
      || syncState.socketGeneration !== expected.socketGeneration
      || syncState.socketId !== expected.socketId
      || syncState.processEpoch !== expected.processEpoch
      || syncState.domainGeneration !== expected.domainGeneration
    ) return false;
    generationRecoveries.push({ expected: { ...expected }, reason });
    syncState = null;
    generationHandler?.(null);
    return true;
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

mock.module("../../src/browser/diag.ts", () => ({
  markPhase: () => undefined,
  markPhaseOnce: () => undefined,
  recordCellLag: () => undefined,
}));

mock.module("../../src/browser/pageVisible.ts", () => ({
  isPageVisible: () => visible,
  pageVisible: () => visible,
  isPageFocused: () => focused,
  pageFocused: () => focused,
}));
// The transport mock must be installed before this singleton registers its
// generation callback; defer loading until Bun has finished evaluating this
// fixture so the reloaded module cannot observe this fixture's export TDZ.
let loadedTerminalStream: typeof TerminalStreamModule | null = null;
const terminalStream = new Proxy({} as TerminalStreamFixtureModule, {
  get: (_target, property) => {
    if (!loadedTerminalStream) throw new Error("terminal stream fixture is not initialized");
    const value = Reflect.get(loadedTerminalStream, property);
    if (
      property === "dispatchTerminalCellFrame"
      || property === "dispatchTerminalCellChunk"
      || property === "dispatchTerminalViewState"
    ) {
      return (frame: unknown): unknown => {
        if (!syncState) return undefined;
        return Reflect.apply(value as (...args: unknown[]) => unknown, undefined, [
          frame,
          { ...syncState, transportKind: "sync", workerFp: null },
        ]);
      };
    }
    return value;
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
const WORKER_FP = "worker-browser-replica";

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
  readonly deltaBatches: CellGridFrame[][] = [];
  mutateRows = false;

  applyFullFrame(frame: CellGridFrame): boolean {
    const owned = cloneCellGridFrame(frame);
    this.fullFrames.push(owned);
    if (this.mutateRows && owned.viewportRows[0]) owned.viewportRows[0].index = 99;
    return true;
  }

  applyDeltaFrames(frames: readonly CellGridFrame[]): boolean {
    this.deltaBatches.push([...frames]);
    this.deltaFrames.push(...frames);
    const first = frames[0];
    if (this.mutateRows && first?.viewportRows[0]) first.viewportRows[0].index = 99;
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

function setPageVisible(next: boolean): void {
  visible = next;
}
function setPageFocused(next: boolean): void {
  focused = next;
}
function dispatchTerminalCellFrameFrom(
  owner: Omit<TestSyncState, "ready">,
  frame: Parameters<typeof TerminalStreamModule.dispatchTerminalCellFrame>[0],
): void {
  if (!loadedTerminalStream) throw new Error("terminal stream fixture is not initialized");
  loadedTerminalStream.dispatchTerminalCellFrame(frame, {
    ...owner,
    transportKind: "sync",
    workerFp: null,
  });
}



beforeEach(() => {
  terminalDirectRegistry.reset("terminal stream fixture reset");
  vi.useFakeTimers();
  terminalStream._resetTerminalStreamForTest();
  sent.length = 0;
  generationRecoveries.length = 0;
  visible = true;
  focused = true;
  syncState = { ...CURRENT_SYNC_OWNER, ready: true };
  generationHandler?.(syncState);
});

afterEach(() => {
  terminalDirectRegistry.reset("terminal stream fixture reset");
  terminalStream._resetTerminalStreamForTest();
  vi.useRealTimers();
});
export {
  CELL_GRID_CHUNK_STALL_MS,
  CURRENT_SYNC_OWNER,
  EPOCH_A,
  RecordingRenderer,
  SESSION_ID,
  SNAPSHOT_A,
  STREAM_A,
  STREAM_B,
  WORKER_FP,
  acceptView,
  cellFrameToProto,
  chunkCellGridFrame,
  dispatchTerminalCellFrameFrom,
  delta,
  full,
  generationRecoveries,
  latestViewCommand,
  rejectView,
  renderer,
  resyncCommands,
  row,
  setPageVisible,
  setPageFocused,
  terminalStream,
  updateSyncState,
  viewCommands,
};
export type { TerminalViewHandleStatus };

