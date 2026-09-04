// Managed Sync still subscribes to the PAIR domain even though pairing is unavailable.
// This regression drives the real bootstrap hydrators and domain-ready publisher against
// one live generation, proving every eager domain crosses its readiness barrier without PairList.

import { expect, test } from "bun:test";
import { fromBinary } from "@bufbuild/protobuf";
import {
  SyncClientFrameSchema,
  SyncDomain,
} from "@roost/shared/proto/sync_pb";
import { reconcile } from "solid-js/store";
import { _installBootstrapDomainHydrators } from "../src/store/sync-bootstrap-hydration.ts";
import {
  _clearLiveSyncLink,
  _installLiveSyncLink,
  type LiveSyncLink,
} from "../src/store/sync-link-state.ts";
import { rootStore, setRootStore } from "../src/store/root.ts";

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

test("managed bootstrap marks every eager domain ready without requesting PairList", async () => {
  const sent: Uint8Array[] = [];
  const calls = {
    sessionsList: 0,
    workersList: 0,
    workspacesList: 0,
    tasksList: 0,
    mcpList: 0,
    pairList: 0,
  };
  const coordClient = {
    sessionsList: async () => {
      calls.sessionsList += 1;
      return { sessions: [], syncSnapshotToken: "terminal-snapshot" };
    },
    workersList: async () => {
      calls.workersList += 1;
      return { workers: [], routableFps: [] };
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
      throw new Error("managed bootstrap must not request PairList");
    },
  };
  const link: LiveSyncLink = {
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
      socketId: "managed-socket",
      processEpoch: "managed-epoch",
      domains: new Map(EAGER_DOMAINS.map((domain) => [domain, {
        generation: 1n,
        subscribed: true,
        ready: false,
      }])),
      routableChunks: new Map(),
    },
  };

  setRootStore("pair_requests", "stale-pair", {
    ephemeral_id: "stale-pair",
    label: "stale",
    created_at_ms: 1,
  });
  _installLiveSyncLink(link);
  const disposeHydrators = _installBootstrapDomainHydrators({
    coordClient: coordClient as never,
    selfHosted: false,
    onTerminalFailure: async (reason) => { throw reason; },
    onTerminalSnapshotApplied: () => {},
    requestReconnect: () => {},
  });

  try {
    await flushHydrators();

    expect(calls).toEqual({
      sessionsList: 1,
      workersList: 1,
      workspacesList: 1,
      tasksList: 1,
      mcpList: 1,
      pairList: 0,
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
  } finally {
    disposeHydrators();
    _clearLiveSyncLink(link);
    setRootStore("pair_requests", reconcile({}));
  }
});
