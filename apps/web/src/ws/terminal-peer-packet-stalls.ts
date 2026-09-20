// Per-lane browser fragment-stall deadlines.
// Traffic on one negotiated channel never extends another channel's partial
// message lifetime; the connection supplies exact assembler expiry callbacks.

import {
  TERMINAL_PEER_LANE_PRIORITY,
  TERMINAL_PEER_PACKET_STALL_MS,
  type TerminalPeerPacketLane,
} from "@roost/shared/terminal-peer";

export class TerminalPeerPacketStalls {
  private readonly timers: Record<TerminalPeerPacketLane, Timer | null> = {
    control: null,
    terminal: null,
    history: null,
  };

  constructor(
    private readonly expireLane: (lane: TerminalPeerPacketLane) => boolean,
    private readonly onStall: () => void,
  ) {}

  update(lane: TerminalPeerPacketLane, hasPartialMessage: boolean): void {
    clearTimeout(this.timers[lane] ?? undefined);
    this.timers[lane] = null;
    if (!hasPartialMessage) return;
    this.timers[lane] = setTimeout(() => {
      this.timers[lane] = null;
      if (this.expireLane(lane)) this.onStall();
    }, TERMINAL_PEER_PACKET_STALL_MS);
  }

  close(): void {
    for (const lane of TERMINAL_PEER_LANE_PRIORITY) {
      clearTimeout(this.timers[lane] ?? undefined);
      this.timers[lane] = null;
    }
  }
}
