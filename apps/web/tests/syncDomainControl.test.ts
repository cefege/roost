import { expect, test } from "bun:test";
import type { SyncV2TerminalState } from "../src/store/sync-link-state.ts";
import {
  _dispatchSyncV2Control,
  registerSyncV2ControlHandler,
  registerSyncV2ProbeResultHandler,
  type SyncV2Control,
} from "../src/store/sync-domain-state.ts";

const state: SyncV2TerminalState = {
  socketGeneration: 1,
  socketId: "socket-1",
  processEpoch: "coord-epoch",
  domainGeneration: 1n,
  ready: true,
};

test("dispatches route controls and isolates probe callbacks", () => {
  const controls: string[] = [];
  const probes: string[] = [];
  const stopControls = registerSyncV2ControlHandler((control) => controls.push(control.case));
  const stopProbes = registerSyncV2ProbeResultHandler((result) => probes.push(result.requestId));
  try {
    _dispatchSyncV2Control({
      case: "inputRouteResult",
      value: {
        requestId: "route-request",
        sessionId: "s1",
        revision: 1n,
        accepted: true,
        latestRevision: 1n,
        inputRouteEpoch: "route-epoch",
        workerEpoch: "worker-epoch",
        reason: "",
      },
    } as SyncV2Control, state);
    _dispatchSyncV2Control({
      case: "terminalTransportProbeResult",
      value: { requestId: "probe-request", workerFp: "worker-a", workerEpoch: "worker-epoch" },
    } as SyncV2Control, state);
    expect(controls).toEqual(["inputRouteResult", "terminalTransportProbeResult"]);
    expect(probes).toEqual(["probe-request"]);
  } finally {
    stopControls();
    stopProbes();
  }
});
