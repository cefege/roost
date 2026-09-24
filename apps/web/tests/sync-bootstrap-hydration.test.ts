// Sync bootstrap domains publish readiness independently within one generation.
// This regression delays WorkersList past terminal hydration so a slow domain
// cannot hold back the ones that already answered.
// A WORKERS domain reset must withdraw readiness until its replacement snapshot lands.
// A domain whose snapshot RPC never settles must be cancelled and retried, or it
// holds its domain un-ready and mutes every mounted terminal for good.

import { afterEach, expect, test, vi } from "bun:test";
import { create, fromBinary } from "@bufbuild/protobuf";
import {
  FirehoseFrameSchema,
  SyncClientFrameSchema,
  SyncDomain,
  SyncDomainResetFrameSchema,
} from "@roost/protocol/proto/sync_pb";
import {
  type WorkersListResponse,
  WorkersListResponseSchema,
} from "@roost/protocol/proto/coordinator_pb";
import { reconcile } from "solid-js/store";
import { _installBootstrapDomainHydrators } from "../src/store/sync-bootstrap-hydration.ts";
import { _consumeSyncFrame } from "../src/store/sync-inbound.ts";
import {
  _clearLiveSyncLink,
  _installLiveSyncLink,
  type LiveSyncLink,
} from "../src/store/sync-link-state.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";
import {
  resetSyncHydration,
  sessionsHydrated,
  workersHydrated,
} from "../src/store/sync-hydrated.ts";
import { SYNC_HYDRATION_DEADLINE_MS } from "../src/store/sync-domain-hydration.ts";

const EAGER_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
] as const;

async function flushHydrators(): Promise<void> {
  for (let turn = 0; turn < 24; turn++) await Promise.resolve();
}

/** One accepting v2 socket with every eager domain subscribed and un-ready:
 *  the exact state a fresh Sync generation hands the hydrators. */
function bootstrapLink(sent: Uint8Array[]): LiveSyncLink {
  return {
    ws: {
      readyState: WebSocket.OPEN,
      send: (data: Uint8Array) => { sent.push(data); },
    } as unknown as WebSocket,
    gen: 7,
    abortReason: null,
    accepting: true,
    resolveClosed: () => {},
    expectsV2: true,
    openTimer: null,
    closeEscapeTimer: null,
    watchdog: null,
    v2: {
      socketId: "bootstrap-socket",
      processEpoch: "bootstrap-epoch",
      domains: new Map(EAGER_DOMAINS.map((domain) => [domain, {
        generation: 1n,
        subscribed: true,
        ready: false,
      }])),
      routableChunks: new Map(),
    },
  };
}

afterEach(() => { vi.useRealTimers(); });

test("bootstrap hydrates and rehydrates workers independently of the other domains", async () => {
  const sent: Uint8Array[] = [];
  const calls = {
    sessionsList: 0,
    workersList: 0,
    workspacesList: 0,
    tasksList: 0,
    mcpList: 0,
    pairList: 0,
  };
  const workerList = Promise.withResolvers<WorkersListResponse>();
  const workerRehydration = Promise.withResolvers<WorkersListResponse>();
  const coordClient = {
    sessionsList: async () => {
      calls.sessionsList += 1;
      return { sessions: [], syncSnapshotToken: "terminal-snapshot" };
    },
    workersList: async () => {
      calls.workersList += 1;
      return calls.workersList === 1 ? workerList.promise : workerRehydration.promise;
    },
    workspacesList: async () => {
      calls.workspacesList += 1;
      return { workspaces: [] };
    },
    tasksList: async () => {
      calls.tasksList += 1;
      return { tasks: [] };
    },
    mcpList: async () => {
      calls.mcpList += 1;
      return { relays: [] };
    },
    pairList: async () => {
      calls.pairList += 1;
      return { requests: [] };
    },
  };
  const link = bootstrapLink(sent);

  setRootStore("pair_requests", "stale-pair", {
    ephemeral_id: "stale-pair",
    label: "stale",
    created_at_ms: 1,
  });
  setRootStore("workers", reconcile({}));
  resetSyncHydration();
  _installLiveSyncLink(link);
  const disposeHydrators = _installBootstrapDomainHydrators({
    coordClient: coordClient as never,
    onTerminalFailure: async (reason) => { throw reason; },
    onTerminalSnapshotApplied: () => {},
    requestReconnect: () => {},
  });

  try {
    await flushHydrators();
    expect(sessionsHydrated()).toBe(true);
    expect(workersHydrated()).toBe(false);
    expect(link.v2?.domains.get(SyncDomain.TERMINAL)?.ready).toBe(true);
    expect(link.v2?.domains.get(SyncDomain.WORKERS)?.ready).toBe(false);

    workerList.resolve(create(WorkersListResponseSchema, {
      workers: [{
        fp: "worker-a",
        label: "Worker A",
        os: "linux",
        registeredAtMs: 1n,
        lastSeenMs: 2n,
      }],
      routableFps: ["worker-a"],
    }));
    await flushHydrators();
    expect(workersHydrated()).toBe(true);
    expect(rootStore.workers["worker-a"]?.label).toBe("Worker A");

    expect(calls).toEqual({
      sessionsList: 1,
      workersList: 1,
      workspacesList: 1,
      tasksList: 1,
      mcpList: 1,
      pairList: 1,
    });
    expect(Object.keys(rootStore.pair_requests)).toEqual([]);
    expect(EAGER_DOMAINS.every((domain) => link.v2?.domains.get(domain)?.ready)).toBe(true);

    const readyDomains = sent.map((bytes) => {
      const command = fromBinary(SyncClientFrameSchema, bytes).command;
      expect(command.case).toBe("domainReady");
      if (command.case !== "domainReady") throw new Error("expected domain-ready command");
      return command.value.domain;
    });
    expect(readyDomains).toHaveLength(EAGER_DOMAINS.length);
    expect([...readyDomains].sort((left, right) => left - right))
      .toEqual([...EAGER_DOMAINS].sort((left, right) => left - right));

    _consumeSyncFrame(link, create(FirehoseFrameSchema, {
      deliverySeq: 0n,
      frame: {
        case: "domainReset",
        value: create(SyncDomainResetFrameSchema, {
          domain: SyncDomain.WORKERS,
          generation: 2n,
          subscribed: true,
        }),
      },
    }));
    expect(workersHydrated()).toBe(false);
    expect(link.v2?.domains.get(SyncDomain.WORKERS)?.ready).toBe(false);
    await flushHydrators();
    expect(calls.workersList).toBe(2);
    expect(workersHydrated()).toBe(false);

    workerRehydration.resolve(create(WorkersListResponseSchema, {
      workers: [{
        fp: "worker-b",
        label: "Worker B",
        os: "linux",
        registeredAtMs: 3n,
        lastSeenMs: 4n,
      }],
      routableFps: ["worker-b"],
    }));
    await flushHydrators();
    expect(workersHydrated()).toBe(true);
    expect(rootStore.workers["worker-b"]?.label).toBe("Worker B");
    expect(link.v2?.domains.get(SyncDomain.WORKERS)?.ready).toBe(true);
    const rehydratedReady = fromBinary(
      SyncClientFrameSchema,
      sent[sent.length - 1]!,
    ).command;
    expect(rehydratedReady.case).toBe("domainReady");
    if (rehydratedReady.case !== "domainReady") {
      throw new Error("expected workers domain-ready command");
    }
    expect(rehydratedReady.value.domain).toBe(SyncDomain.WORKERS);
    expect(rehydratedReady.value.generation).toBe(2n);
  } finally {
    disposeHydrators();
    _clearLiveSyncLink(link);
    setRootStore("pair_requests", reconcile({}));
    setRootStore("workers", reconcile({}));
    resetSyncHydration();
  }
});

test("a hydration that never settles is cancelled and retried", async () => {
  vi.useFakeTimers();
  const sent: Uint8Array[] = [];
  const abortSignals: AbortSignal[] = [];
  let sessionsListCalls = 0;
  const coordClient = {
    // A pooled half-open connection after a sleeping laptop wakes: the request
    // is written and nothing ever answers it.
    sessionsList: (_request: unknown, options: { signal: AbortSignal }) => {
      sessionsListCalls += 1;
      abortSignals.push(options.signal);
      return Promise.withResolvers<never>().promise;
    },
    workersList: async () => ({ workers: [], routableFps: [] }),
    workspacesList: async () => ({ workspaces: [] }),
    tasksList: async () => ({ tasks: [] }),
    mcpList: async () => ({ relays: [] }),
    pairList: async () => ({ requests: [] }),
  };
  const link = bootstrapLink(sent);

  resetSyncHydration();
  _installLiveSyncLink(link);
  const disposeHydrators = _installBootstrapDomainHydrators({
    coordClient: coordClient as never,
    onTerminalFailure: async () => {},
    onTerminalSnapshotApplied: () => {},
    requestReconnect: () => {},
  });

  try {
    await flushHydrators();
    expect(sessionsListCalls).toBe(1);
    expect(sessionsHydrated()).toBe(false);
    expect(abortSignals[0]?.aborted).toBe(false);

    vi.advanceTimersByTime(SYNC_HYDRATION_DEADLINE_MS);
    await flushHydrators();
    expect(abortSignals[0]?.aborted).toBe(true);
    expect(sessionsHydrated()).toBe(false);
    expect(link.v2?.domains.get(SyncDomain.TERMINAL)?.ready).toBe(false);
    // Pre-fix the hydrator was called once and never again: the deadline is
    // what turns an unsettled snapshot back into the ordinary retry.
    expect(sessionsListCalls).toBe(1);

    vi.advanceTimersByTime(500);
    await flushHydrators();
    expect(sessionsListCalls).toBe(2);
    expect(link.v2?.domains.get(SyncDomain.TERMINAL)?.ready).toBe(false);
    // Every other domain answered inside the same generation.
    expect(link.v2?.domains.get(SyncDomain.WORKSPACES)?.ready).toBe(true);
  } finally {
    disposeHydrators();
    _clearLiveSyncLink(link);
    setRootStore("pair_requests", reconcile({}));
    setRootStore("workers", reconcile({}));
    resetSyncHydration();
  }
});
