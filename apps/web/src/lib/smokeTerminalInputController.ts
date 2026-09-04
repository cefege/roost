// Browser smoke checks need raw terminal input without relying on synthetic keyboard events.
// This controller sends input through the real outbound transport and records accepted batches.
// Its bounded capture prevents long stress runs from retaining unbounded terminal payloads.
// The smoke backdoor creates one controller for each installed browser API.

import { sendTerminalInput, setSmokeTerminalInputObserver } from "../ws/sync-outbound.ts";
import type { SmokeApi } from "./smokeTypes.ts";

type SmokeTerminalInputMethods = Pick<
  SmokeApi,
  "input" | "terminalInputCapture" | "resetTerminalInputCapture"
>;

export function createSmokeTerminalInputMethods(): SmokeTerminalInputMethods {
  const terminalInputBatches: Array<{ sessionId: string; data: Uint8Array }> = [];
  let terminalInputBytes = 0;
  let droppedTerminalInputBatches = 0;

  const clearTerminalInputCapture = () => {
    terminalInputBatches.length = 0;
    terminalInputBytes = 0;
    droppedTerminalInputBatches = 0;
  };

  setSmokeTerminalInputObserver((sessionId, data) => {
    if (data.byteLength > TERMINAL_INPUT_CAPTURE_MAX_BYTES) {
      droppedTerminalInputBatches++;
      return;
    }
    while (
      terminalInputBatches.length >= TERMINAL_INPUT_CAPTURE_MAX_BATCHES
      || terminalInputBytes + data.byteLength > TERMINAL_INPUT_CAPTURE_MAX_BYTES
    ) {
      const evicted = terminalInputBatches.shift();
      if (!evicted) break;
      terminalInputBytes -= evicted.data.byteLength;
      droppedTerminalInputBatches++;
    }
    terminalInputBatches.push({ sessionId, data });
    terminalInputBytes += data.byteLength;
  });

  return {
    async input(sessionId, text) {
      const admission = sendTerminalInput(sessionId, new TextEncoder().encode(text));
      if (!admission.accepted) throw new Error(admission.reason);
      const outcome = await admission.result;
      if (outcome.status !== "accepted") throw new Error(outcome.reason);
    },
    terminalInputCapture() {
      return {
        batches: terminalInputBatches.map(({ sessionId, data }) => ({
          sessionId,
          data: Array.from(data),
        })),
        droppedBatches: droppedTerminalInputBatches,
      };
    },
    resetTerminalInputCapture() {
      clearTerminalInputCapture();
    },
  };
}

const TERMINAL_INPUT_CAPTURE_MAX_BATCHES = 512;
const TERMINAL_INPUT_CAPTURE_MAX_BYTES = 1024 * 1024;
