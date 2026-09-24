// Sync worker-control probes measure the authenticated control path without terminal bytes.
// Smoke and diagnostics use this owner for the Sync carrier; direct adapters retain their own probes.
// Waiters and samples are socket-fenced so an old result cannot describe a replacement connection.
import { create } from "@bufbuild/protobuf";
import {
  TerminalTransportProbeSchema,
  type TerminalTransportProbeResult,
} from "@roost/protocol/proto/sync_pb";
import {
  currentSyncV2TerminalState,
  registerSyncV2GenerationHandler,
  type SyncV2TerminalState,
} from "../store/sync-link-state.ts";
import {
  registerSyncV2ProbeResultHandler,
  sendSyncV2Command,
} from "../store/sync-domain-state.ts";
import { createTerminalDirectRequestId, terminalDirectMonotonicNow } from "./terminal-direct-browser.ts";

const MAX_RETAINED_CONTROL_TELEMETRY_WORKERS = 64;
const CONTROL_PROBE_TIMEOUT_MS = 3_000;
const MAX_PENDING_CONTROL_PROBES = 32;

export interface SyncTerminalWorkerControlTelemetry {
  readonly workerEpoch: string;
  readonly lastProbeAtMs: number;
  readonly controlRttMs: number;
}

type PendingControlProbe = {
  readonly workerFp: string;
  readonly connectionKey: string;
  readonly startedAtMs: number;
  readonly resolve: (value: SyncTerminalWorkerControlTelemetry) => void;
  readonly reject: (reason: Error) => void;
  readonly timer: Timer;
};

const pendingControlProbes = new Map<string, PendingControlProbe>();
const telemetryByWorker = new Map<string, SyncTerminalWorkerControlTelemetry & { connectionKey: string }>();

/** Sends one bounded content-free probe over the current authenticated Sync socket. */
export function probeSyncTerminalWorker(workerFp: string): Promise<SyncTerminalWorkerControlTelemetry> {
  const state = currentSyncV2TerminalState();
  if (!state?.ready) return Promise.reject(new Error("terminal Sync is not connected"));
  if (workerFp.length === 0 || workerFp.length > 128) return Promise.reject(new Error("invalid terminal worker identity"));
  if (pendingControlProbes.size >= MAX_PENDING_CONTROL_PROBES) return Promise.reject(new Error("terminal control probe capacity reached"));
  const requestId = createTerminalDirectRequestId();
  const startedAtMs = terminalDirectMonotonicNow();
  const { promise, resolve, reject } = Promise.withResolvers<SyncTerminalWorkerControlTelemetry>();
  const timer = setTimeout(() => {
    pendingControlProbes.delete(requestId);
    reject(new Error("terminal Sync control probe timed out"));
  }, CONTROL_PROBE_TIMEOUT_MS);
  pendingControlProbes.set(requestId, {
    workerFp,
    connectionKey: syncConnectionKey(state),
    startedAtMs,
    resolve,
    reject,
    timer,
  });
  if (sendSyncV2Command({
    case: "terminalTransportProbe",
    value: create(TerminalTransportProbeSchema, { requestId, workerFp }),
  })) return promise;
  rejectPendingControlProbe(requestId, "terminal Sync did not accept the control probe");
  return promise;
}

/** Returns the latest content-free worker control RTT for this exact Sync socket. */
export function syncTerminalWorkerControlTelemetry(
  workerFp: string,
  state = currentSyncV2TerminalState(),
): SyncTerminalWorkerControlTelemetry | null {
  if (!state?.ready) return null;
  const sample = telemetryByWorker.get(workerFp);
  if (!sample || sample.connectionKey !== syncConnectionKey(state)) return null;
  return {
    workerEpoch: sample.workerEpoch,
    lastProbeAtMs: sample.lastProbeAtMs,
    controlRttMs: sample.controlRttMs,
  };
}
/** Clears socket-fenced samples at a credential boundary before another identity can reuse them. */
export function resetSyncTerminalControlProbes(reason: string): void {
  for (const requestId of [...pendingControlProbes.keys()]) {
    rejectPendingControlProbe(requestId, reason);
  }
  telemetryByWorker.clear();
}


function syncConnectionKey(state: SyncV2TerminalState): string {
  return JSON.stringify([state.socketId, state.processEpoch]);
}

function resolveControlProbe(
  result: TerminalTransportProbeResult,
  state: SyncV2TerminalState,
): void {
  const pending = pendingControlProbes.get(result.requestId);
  if (
    !pending
    || result.workerFp !== pending.workerFp
    || result.workerEpoch.length === 0
    || pending.connectionKey !== syncConnectionKey(state)
  ) return;
  pendingControlProbes.delete(result.requestId);
  clearTimeout(pending.timer);
  const receivedAtMs = terminalDirectMonotonicNow();
  const sample = {
    workerEpoch: result.workerEpoch,
    lastProbeAtMs: receivedAtMs,
    controlRttMs: Math.max(0, receivedAtMs - pending.startedAtMs),
  };
  retainWorkerTelemetry(result.workerFp, { ...sample, connectionKey: pending.connectionKey });
  pending.resolve(sample);
}

function retainWorkerTelemetry(
  workerFp: string,
  sample: SyncTerminalWorkerControlTelemetry & { connectionKey: string },
): void {
  if (!telemetryByWorker.has(workerFp) && telemetryByWorker.size >= MAX_RETAINED_CONTROL_TELEMETRY_WORKERS) {
    const oldestWorkerFp = telemetryByWorker.keys().next().value;
    if (oldestWorkerFp !== undefined) telemetryByWorker.delete(oldestWorkerFp);
  }
  telemetryByWorker.delete(workerFp);
  telemetryByWorker.set(workerFp, sample);
}

function rejectPendingControlProbe(requestId: string, reason: string): void {
  const pending = pendingControlProbes.get(requestId);
  if (!pending) return;
  pendingControlProbes.delete(requestId);
  clearTimeout(pending.timer);
  pending.reject(new Error(reason));
}

function discardSupersededControlProbes(state: SyncV2TerminalState | null): void {
  const connectionKey = state ? syncConnectionKey(state) : null;
  for (const [requestId, pending] of pendingControlProbes) {
    if (pending.connectionKey !== connectionKey) {
      rejectPendingControlProbe(requestId, "terminal Sync connection changed");
    }
  }
  for (const [workerFp, sample] of telemetryByWorker) {
    if (sample.connectionKey !== connectionKey) telemetryByWorker.delete(workerFp);
  }
}

registerSyncV2ProbeResultHandler(resolveControlProbe);
registerSyncV2GenerationHandler(discardSupersededControlProbes);
