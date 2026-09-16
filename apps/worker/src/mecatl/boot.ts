// Boots this machine's Mecatl runtime: the supervised daemon plus the relay
// that answers coordinator-forwarded HTTP through it. Called once from the
// worker's boot sequence, which owns the link the relay answers on and the
// heartbeat that publishes the daemon's state to operators.

import type { MecatlRuntimeReport } from "@roost/shared/mecatl-runtime";
import type { WorkerConfig } from "../config.ts";
import type { CoordLink } from "../transport/coord-link-types.ts";
import { createMecatlDaemon, type MecatlDaemon } from "./daemon.ts";
import { createMecatlRelay, type MecatlRelay } from "./relay.ts";

export interface MecatlRuntime {
  daemon: MecatlDaemon;
  relay: MecatlRelay;
  /** The heartbeat's view of the daemon, for `roost status` and `roost doctor`. */
  report(): MecatlRuntimeReport;
}

/** The agent runtime is independent of every PTY subsystem: an unavailable
 *  daemon only makes the agent surface unavailable, never the terminals. */
export function startMecatlRuntime(cfg: WorkerConfig, link: CoordLink): MecatlRuntime {
  const daemon = createMecatlDaemon(cfg);
  const relay = createMecatlRelay({
    daemon,
    send: (chunk) => link.sendMecatlRelayChunk(chunk),
  });
  return {
    daemon,
    relay,
    report: () => {
      const state = daemon.state();
      return state.kind === "unavailable"
        ? { state: "unavailable", reason: state.reason }
        : { state: state.kind };
    },
  };
}
