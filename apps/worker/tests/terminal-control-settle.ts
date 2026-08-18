// Drain a channel's terminal-control lane.
//
// A viewport claim is transactional: it installs the claim synchronously and
// queues the SCD reconcile — keeper resize plus the single core rebuild — on the
// per-channel control lane. "The resize finished" is therefore the lane going
// idle, not the claim call returning. Each queued control can enqueue the next
// (a claim's reconcile, a reaper's), so drain until the lane record is gone.

import type { SessionManager } from "../src/session-manager.ts";

export async function settleTerminalControl(mgr: SessionManager, channelId: number): Promise<void> {
  for (let i = 0; i < 50; i++) {
    const lane = mgr.terminalControlChains.get(channelId);
    if (!lane) return;
    await lane.tail;
  }
}
