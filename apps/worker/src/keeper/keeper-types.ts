// Shared keeper state shapes. Split out of multiplexed-main.ts so the entry,
// the frame handler, and the process-reaper all reference one definition
// instead of re-declaring these interfaces.

import type * as net from "node:net";
import type { SbRing } from "../session-scrollback-ring.ts";
import type { ResizeWireResult } from "./protocol-v2.ts";
import type { WindowsJobHostHandle } from "@roost/shared/windows-helper";

export interface KeeperHistoryResize {
  /** Raw-output sequence at which the new dimensions became effective. */
  headSeq: number;
  seq: number;
  cols: number;
  rows: number;
}

export interface KeeperInputBatch {
  /** null identifies the unacknowledged legacy PtyIn frame. */
  inputSeq: number | null;
  bytes: Buffer;
  socket: net.Socket | null;
  started: boolean;
}

export interface KeeperOutputBoundaryBuffer {
  chunks: Buffer[];
  bytes: number;
}

export interface Channel {
  proc: Bun.Subprocess;
  terminal: Bun.Terminal;
  // Windows channels are owned by the helper's kill-on-close Job Object.
  // childPid is the assigned shell pid; proc is the job-host process.
  jobHost?: WindowsJobHostHandle;
  childPid: number;
  exited: boolean;
  // Sliding window of recent raw PtyOut bytes. headSeq counts every raw byte
  // ever released to workers; resize records do not advance it.
  outRing: SbRing;
  headSeq: number;
  // Dimensions immediately before the first retained ordered record.
  historyBaseCols: number;
  historyBaseRows: number;
  historyResizes: KeeperHistoryResize[];
  currentCols: number;
  currentRows: number;
  // Non-null only across terminal.resize → ACK. PTY callbacks append here so
  // post-resize bytes cannot overtake the resize boundary.
  outputBoundaryBuffer: KeeperOutputBoundaryBuffer | null;
  // Idempotence/status-query cache for logical resize sequences.
  resizeStatuses: Map<number, ResizeWireResult>;
  highestResizeSeq: number;
  // One per-channel FIFO: the head is repeatedly short-written to completion
  // before a later batch may call terminal.write.
  inputQueue: KeeperInputBatch[];
  inputQueueBytes: number;
  inputWriting: boolean;
}

export interface ClientState {
  buf: Buffer;
  socket: net.Socket;
}
