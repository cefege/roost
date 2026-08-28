import { create } from "@bufbuild/protobuf";
import {
  TerminalViewCommandSchema,
  type FirehoseFrame,
  type TerminalViewCommand,
} from "@roost/shared/proto/sync_pb";
import {
  TerminalStreamFailureKind,
  TerminalStreamStatus,
  WTerminalStreamResultSchema,
  type WTerminalStreamResult,
} from "@roost/shared/proto/worker_transport_pb";
import {
  TerminalViewHub,
  type TerminalViewHubOptions,
} from "../src/connect/terminal-view-hub.ts";
import type { TerminalScreenSocketSink } from "../src/connect/terminal-screen-hub.ts";

export const SESSION = "10000000-0000-4000-8000-000000000001";
export const OTHER_SESSION = "10000000-0000-4000-8000-000000000002";
export const VIEW_A = "20000000-0000-4000-8000-000000000001";
export const VIEW_B = "20000000-0000-4000-8000-000000000002";
export const WORKER = "worker-a";
export const MAX_U64 = (1n << 64n) - 1n;

type StreamSender = NonNullable<TerminalViewHubOptions["sendStreamState"]>;
export type StreamState = Parameters<StreamSender>[1];
type ResolveRoute = NonNullable<TerminalViewHubOptions["resolveRoute"]>;
export type Route = Awaited<ReturnType<ResolveRoute>>;
type SentStream = StreamState & { workerFp: string };

export class TestSink implements TerminalScreenSocketSink {
  readonly begins: Array<[sessionId: string, streamId: string]> = [];
  readonly states: Array<{ frame: FirehoseFrame; sessionId: string }> = [];
  readonly snapshots: Array<{
    sessionId: string;
    streamId: string;
    frames: readonly FirehoseFrame[];
  }> = [];
  readonly deltas: Array<{ sessionId: string; streamId: string; frame: FirehoseFrame }> = [];
  readonly drops: string[] = [];
  private readonly lanes = new Map<string, string>();

  beginTerminalStream(sessionId: string, streamId: string): boolean {
    if (this.lanes.get(sessionId) === streamId) return false;
    this.lanes.set(sessionId, streamId);
    this.begins.push([sessionId, streamId]);
    return true;
  }

  enqueueTerminalState(frame: FirehoseFrame, sessionId: string): void {
    this.states.push({ frame, sessionId });
  }

  replaceTerminalSnapshot(
    sessionId: string,
    streamId: string,
    frames: readonly FirehoseFrame[],
  ): void {
    this.snapshots.push({ sessionId, streamId, frames });
  }

  enqueueTerminalDelta(sessionId: string, streamId: string, frame: FirehoseFrame): boolean {
    this.deltas.push({ sessionId, streamId, frame });
    return true;
  }

  dropTerminalSession(sessionId: string): void {
    this.lanes.delete(sessionId);
    this.drops.push(sessionId);
  }
}

export function uuid(index: number): string {
  return `30000000-0000-4000-8000-${index.toString(16).padStart(12, "0")}`;
}

export function viewCommand(
  viewId: string,
  revision: bigint,
  overrides: Partial<{
    sessionId: string;
    cols: number;
    rows: number;
    active: boolean;
  }> = {},
): TerminalViewCommand {
  return create(TerminalViewCommandSchema, {
    viewId,
    sessionId: overrides.sessionId ?? SESSION,
    cols: overrides.cols ?? 80,
    rows: overrides.rows ?? 24,
    revision,
    active: overrides.active ?? true,
  });
}

export function terminalStates(sink: TestSink) {
  return sink.states.map(({ frame }) => {
    if (frame.frame.case !== "terminalViewState") {
      throw new Error(`expected terminal view state, got ${frame.frame.case}`);
    }
    return frame.frame.value;
  });
}

export function statesFor(sink: TestSink, viewId: string) {
  return terminalStates(sink).filter((state) => state.viewId === viewId);
}

export function resultFor(
  state: StreamState,
  status = TerminalStreamStatus.COMMITTED,
  failureKind = TerminalStreamFailureKind.UNSPECIFIED,
): WTerminalStreamResult {
  return create(WTerminalStreamResultSchema, {
    sessionId: state.sessionId,
    streamId: state.streamId,
    enabled: state.enabled,
    status,
    failureKind,
    effectiveCols: state.cols,
    effectiveRows: state.rows,
  });
}

export function admitted(result: Promise<WTerminalStreamResult>) {
  return { admitted: true, expired: false, requestId: null, result };
}

export function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

interface HarnessOptions {
  clock?: { value: number };
  resolveRoute?: ResolveRoute;
  sendStreamState?: StreamSender;
  sendSnapshot?: NonNullable<TerminalViewHubOptions["sendSnapshot"]>;
}

const liveHubs: TerminalViewHub[] = [];

export function disposeHubs(): void {
  for (const hub of liveHubs.splice(0)) hub.dispose();
}

export function makeHarness(options: HarnessOptions = {}) {
  const clock = options.clock ?? { value: 1_000 };
  const sent: SentStream[] = [];
  const routeCalls: string[] = [];
  const snapshotRequests: Array<{ workerFp: string; sessionId: string; streamId: string }> = [];
  const resolveRoute: ResolveRoute = options.resolveRoute
    ?? (async () => ({ workerFp: WORKER, channel: 7 }));
  const sendStreamState: StreamSender = options.sendStreamState
    ?? ((_workerFp, state) => admitted(Promise.resolve(resultFor(state))));
  const sendSnapshot = options.sendSnapshot
    ?? ((_workerFp: string, _sessionId: string, _streamId: string) => true);
  const hub = new TerminalViewHub({
    db: undefined as never,
    now: () => clock.value,
    resolveRoute: async (sessionId) => {
      routeCalls.push(sessionId);
      return resolveRoute(sessionId);
    },
    sendStreamState: (workerFp, state) => {
      sent.push({ workerFp, ...state });
      return sendStreamState(workerFp, state);
    },
    sendSnapshot: (workerFp, sessionId, streamId) => {
      snapshotRequests.push({ workerFp, sessionId, streamId });
      return sendSnapshot(workerFp, sessionId, streamId);
    },
  });
  liveHubs.push(hub);
  return { hub, clock, sent, routeCalls, snapshotRequests };
}

export function register(
  hub: TerminalViewHub,
  socketId = "socket-a",
  viewerKey: string | null = "viewer-a",
  fingerprint = "fingerprint-a",
): TestSink {
  const sink = new TestSink();
  hub.registerSocket({ socketId, viewerKey, callerFingerprint: fingerprint, sink });
  return sink;
}

export function sweep(hub: TerminalViewHub): void {
  (hub as unknown as { sweep(): void }).sweep();
}

export async function settle(): Promise<void> {
  for (let index = 0; index < 8; index += 1) await Promise.resolve();
}
