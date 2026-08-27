import type { CellData, TerminalCore } from "@wterm/core";
import {
  DEFAULT_COLOR,
  initCellEmitState,
} from "@roost/shared/cell";
import type {
  PbCellGridChunk,
  PbCellGridFrame,
} from "@roost/shared/proto/cell_pb";
import { createWtermCore } from "@roost/shared/wterm-core-factory";
import {
  asChannelId,
  asSessionId,
  asWorkerFp,
} from "@roost/shared/wire";
import type { FsmChannel } from "../src/fsm.ts";
import { getMultiplexedPool } from "../src/keeper/multiplexed-client.ts";
import { SessionManager } from "../src/session-manager.ts";
import type { SessionShellRecord } from "../src/session-record.ts";
import { createSbRing } from "../src/session-scrollback-ring.ts";
import { initAgentOscState } from "../src/terminal-stream-scan.ts";
import type { TerminalCellSendResult } from "../src/transport/coord-link-types.ts";
import type { FakeKeeper } from "./keeper-fake-pool.ts";
import { keeperTestShellSpec } from "./keeper-test-fixtures.ts";

export const SESSION_ID = asSessionId("11111111-2222-4333-8444-555555555555");
export const CHANNEL_ID = asChannelId(43_211);
const BOOT_STREAM = "00000000-0000-4000-8000-000000000001";
export const STREAM_A = "00000000-0000-4000-8000-00000000000a";
export const STREAM_B = "00000000-0000-4000-8000-00000000000b";
export const STREAM_C = "00000000-0000-4000-8000-00000000000c";
export const TEST_COLS = 12;
export const TEST_ROWS = 6;

interface HarnessOptions {
  sendFrame?: (frame: PbCellGridFrame) => TerminalCellSendResult;
  sendChunk?: (chunk: PbCellGridChunk) => TerminalCellSendResult;
}

export interface StreamHarness {
  manager: SessionManager;
  record: SessionShellRecord;
  frameAttempts: PbCellGridFrame[];
  chunkAttempts: PbCellGridChunk[];
}

const managers: SessionManager[] = [];
const keepers: FakeKeeper[] = [];
let requestOrdinal = 0;

export function trackKeeper(keeper: FakeKeeper): FakeKeeper {
  keepers.push(keeper);
  return keeper;
}

export async function makeHarness(
  suppliedCore?: TerminalCore,
  options: HarnessOptions = {},
): Promise<StreamHarness> {
  const core = suppliedCore ?? await createWtermCore(TEST_COLS, TEST_ROWS);
  const frameAttempts: PbCellGridFrame[] = [];
  const chunkAttempts: PbCellGridChunk[] = [];
  const manager = new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: { emit: () => {} },
    sendBinaryUpstream: () => "sent",
    sendCellGridUpstream: (_channelId, frame) => {
      frameAttempts.push(frame);
      return options.sendFrame?.(frame) ?? "sent";
    },
    sendCellGridChunkUpstream: (_channelId, chunk) => {
      chunkAttempts.push(chunk);
      return options.sendChunk?.(chunk) ?? "sent";
    },
  });
  managers.push(manager);
  const record: SessionShellRecord = {
    sessionId: SESSION_ID,
    channelId: CHANNEL_ID,
    socketPath: "/dev/null",
    kind: "shell",
    cwd: "/",
    shellSpec: keeperTestShellSpec({ executable: process.execPath, cwd: "/" }),
    fsm: {} as unknown as FsmChannel,
    scrollback: createSbRing(),
    head_seq: 0,
    alt_mode: false,
    mode_carry: new Uint8Array(0),
    osc7_carry: new Uint8Array(0),
    query_carry: new Uint8Array(0),
    ...initAgentOscState(),
    wtermCore: core,
    session_trace_id: "stream-state-test",
    cell_emit: initCellEmitState("stream-state-grid", BOOT_STREAM),
    lastPtyOutMs: 0,
    sb_origin_pin: null,
    spawnedAtMs: Date.now(),
  };
  manager.sessions.set(CHANNEL_ID, record);
  manager.lastAppliedSize.set(CHANNEL_ID, {
    cols: core.getCols(),
    rows: core.getRows(),
  });
  return { manager, record, frameAttempts, chunkAttempts };
}

export function enableStream(
  manager: SessionManager,
  streamId: string,
  cols = TEST_COLS,
  rows = TEST_ROWS,
) {
  requestOrdinal += 1;
  return manager.applyTerminalStreamState({
    requestId: `terminal-stream-test-${requestOrdinal}`,
    sessionId: SESSION_ID,
    streamId,
    enabled: true,
    cols,
    rows,
  });
}

export function frameRowText(frame: PbCellGridFrame, row: number): string {
  return frame.viewportRows
    .find((candidate) => candidate.index === row)
    ?.spans.map((span) => span.text).join("") ?? "";
}

export function coreRowText(core: TerminalCore, row: number): string {
  let text = "";
  for (let col = 0; col < core.getCols(); col += 1) {
    const cell = core.getCell(row, col);
    text += cell.chars ?? String.fromCodePoint(cell.char || 0x20);
  }
  return text.trimEnd();
}

export function paintRows(core: TerminalCore, rows: readonly string[]): void {
  for (let row = 0; row < rows.length; row += 1) {
    core.writeString(`\x1b[${row + 1};1H${rows[row]}`);
  }
}

export async function flushLeadingCellEmit(): Promise<void> {
  await new Promise<void>((resolve) => queueMicrotask(resolve));
}

export function cleanupStreamHarnesses(): void {
  for (const manager of managers.splice(0).reverse()) {
    manager._disposeOutputState(CHANNEL_ID);
    for (const timer of manager.cellEmitTimers.values()) if (timer !== null) clearTimeout(timer);
    manager.cellEmitTimers.clear();
    manager.sessions.clear();
    manager.dispose();
  }
  for (const keeper of keepers.splice(0).reverse()) keeper.restore();
  getMultiplexedPool().forgetInputSequence(CHANNEL_ID);
}

export class DenseLinkedCore {
  private dirty = true;
  private readonly uri = `https://chunk.test/${"x".repeat(112)}`;
  private readonly cells: readonly [CellData, CellData] = [
    {
      char: 0x58,
      width: 1,
      fg: DEFAULT_COLOR,
      bg: DEFAULT_COLOR,
      flags: 0,
      fgRgb: undefined,
      bgRgb: undefined,
      linkUri: this.uri,
      linkKey: "dense-a",
    },
    {
      char: 0x58,
      width: 1,
      fg: DEFAULT_COLOR,
      bg: DEFAULT_COLOR,
      flags: 0,
      fgRgb: undefined,
      bgRgb: undefined,
      linkUri: this.uri,
      linkKey: "dense-b",
    },
  ];

  getCols(): number { return 256; }
  getRows(): number { return 256; }
  getCell(_row: number, col: number): CellData { return this.cells[col & 1]!; }
  getCursor() { return { row: 0, col: 0, visible: true }; }
  usingAltScreen(): boolean { return false; }
  cursorKeysApp(): boolean { return false; }
  bracketedPaste(): boolean { return false; }
  getScrollbackCount(): number { return 0; }
  getScrollbackDiscardedCount(): number { return 0; }
  isDirtyRow(): boolean { return this.dirty; }
  clearDirty(): void { this.dirty = false; }
  markAllDirty(): void { this.dirty = true; }
}
