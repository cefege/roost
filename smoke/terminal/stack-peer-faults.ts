// Public disposable peer fault controls composed by the terminal smoke stack.
// Each method routes to a named source worker through the stack-only Unix socket.
// The direct input hold is separate because it waits at authenticated packet ingress.
// No product client, coordinator RPC, or environment can import these controls.

import type { DirectInputHold } from "./stack-direct-input-hold.ts";
import type { PeerFaultOfferKind, StackPeerFaultControl } from "./stack-peer-fault-control.ts";
import type { TerminalPeerFaults } from "./stack-types.ts";

export function createTerminalPeerFaults(
  directInputHold: DirectInputHold,
  control: StackPeerFaultControl,
): TerminalPeerFaults {
  return {
    holdNextDirectInput: (sessionId) => directInputHold.holdNextInput(sessionId),
    setPeerPacketBlackhole: async (workerLabel, enabled) => {
      await control.command<void>(workerLabel, "set_packet_blackhole", enabled);
    },
    dropNextDirectInputResult: async (workerLabel) => {
      await control.command<void>(workerLabel, "drop_next_input_result");
    },
    advanceGrantClock: async (workerLabel, milliseconds) => {
      await control.command<void>(workerLabel, "advance_grant_clock", milliseconds);
    },
    shrinkGrantForSession: async (workerLabel, sessionId) =>
      await control.command<number>(workerLabel, "shrink_grant_session", sessionId),
    holdNextDirectHistoryResponse: async (workerLabel, sessionId) => {
      const holdId = await control.command<string>(workerLabel, "hold_history_response", sessionId);
      return {
        release: async () => {
          await control.command<void>(workerLabel, "release_history_response", holdId);
        },
        drop: async () => {
          await control.command<void>(workerLabel, "drop_history_response", holdId);
        },
      };
    },
    injectMalformedDirectPacket: async (workerLabel, kind) => {
      await control.command<void>(workerLabel, "inject_malformed_packet", kind);
    },
    setDirectHistoryPaused: async (workerLabel, paused) => {
      await control.command<void>(workerLabel, "set_history_paused", paused);
    },
    holdKeeperAdmission: async (workerLabel, sessionId) => {
      const holdId = await control.command<string>(workerLabel, "hold_keeper_admission", sessionId);
      return {
        release: async () => {
          await control.command<void>(workerLabel, "release_keeper_admission", holdId);
        },
      };
    },
    dropNextDirectRetire: async (workerLabel) => {
      await control.command<void>(workerLabel, "drop_next_direct_retire");
    },
    armNextOfferFault: async (workerLabel, fault: PeerFaultOfferKind) => {
      await control.command<void>(workerLabel, "arm_offer_fault", fault);
    },
  };
}
