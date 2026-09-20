// Browser-side retained-byte accounting for one WebRTC terminal peer.
// TerminalPeerConnection gives each packet queue a lane-bound quota from here;
// queue ownership releases each reservation exactly once on drain or close.

import type {
  TerminalPeerPacketLane,
  TerminalPeerPacketQuota,
} from "@roost/shared/terminal-peer-packets";

export class BrowserTerminalPeerPacketBudget {
  private controlBytes = 0;
  private applicationBytes = 0;

  quota(lane: TerminalPeerPacketLane): TerminalPeerPacketQuota {
    return {
      reserve: (bytes) => {
        if (lane === "control") {
          if (this.controlBytes + bytes > 256 * 1024) return false;
          this.controlBytes += bytes;
        } else {
          if (this.applicationBytes + bytes > 64 * 1024 * 1024) return false;
          this.applicationBytes += bytes;
        }
        return true;
      },
      release: (bytes) => {
        if (lane === "control") this.controlBytes -= bytes;
        else this.applicationBytes -= bytes;
      },
    };
  }
}
