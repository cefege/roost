// Alternate source entrypoint used only by terminal peer smoke stacks.
// It injects disposable Unix-socket callbacks and state into runWorker before boot.
// Product main and compiled workers never import this entrypoint or expose its control.
// The input callback runs after authenticated reassembly and before LocalTerminalSockets writes.

import { runWorker } from "../../apps/worker/src/main.ts";
import { TerminalPeerTestFaultState } from "../../apps/worker/src/terminal/peer/terminal-peer-test-faults.ts";
import { awaitDirectInputHoldDecision } from "./stack-direct-input-hold.ts";
import { startWorkerPeerFaultClient } from "./stack-peer-fault-worker-client.ts";

const directInputArgumentPrefix = "--direct-input-hold-socket=";
const peerFaultArgumentPrefix = "--terminal-peer-fault-socket=";
const directInputSocketPath = process.argv
  .find((argument) => argument.startsWith(directInputArgumentPrefix))
  ?.slice(directInputArgumentPrefix.length);
const peerFaultSocketPath = process.argv
  .find((argument) => argument.startsWith(peerFaultArgumentPrefix))
  ?.slice(peerFaultArgumentPrefix.length);
if (!directInputSocketPath || !peerFaultSocketPath) {
  throw new Error("terminal peer smoke entry requires its control sockets");
}

const terminalPeerTestFaults = new TerminalPeerTestFaultState();
const peerFaultClient = await startWorkerPeerFaultClient(peerFaultSocketPath, terminalPeerTestFaults);
await runWorker({
  localTerminalTestFaults: {
    onAuthenticatedPeerInput: (_port, frame) =>
      awaitDirectInputHoldDecision(directInputSocketPath, frame.sessionId, frame.inputSeq),
  },
  terminalPeerTestFaults,
  onTerminalPeerTestReady: (sessions) => { peerFaultClient.setSessions(sessions); },
});
