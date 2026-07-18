// Shared keeper state shapes. Split out of multiplexed-main.ts so the entry,
// the frame handler, and the process-reaper all reference one definition
// instead of re-declaring these interfaces.

import type * as net from "node:net";

export interface Channel {
  proc: Bun.Subprocess;
  exited: boolean;
  // Sliding window of recent PtyOut bytes. head_seq = total bytes ever
  // output (monotonic over channel lifetime, NOT bytes retained);
  // tail_seq = head_seq - outRing.length. Appended in the SAME callback
  // that broadcasts, so head_seq stays consistent with the worker's
  // appendScrollback count (both see every broadcast chunk exactly once).
  outRing: Buffer;
  headSeq: number;
}

export interface ClientState {
  buf: Buffer;
  socket: net.Socket;
}
