// Publication selection keeps Sync available when no direct route has committed.
// This isolates the fallback from websocket adapters and verifies the token's
// explicit Sync carrier fields rather than relying on a process-epoch sentinel.

import { afterEach, describe, expect, mock, test } from "bun:test";
import type { TerminalViewCommand } from "@roost/shared/proto/sync_pb";
import { terminalDirectRegistry } from "../src/store/terminal-stream-transport.ts";

interface TestSyncState {
  socketGeneration: number;
  socketId: string;
  processEpoch: string;
  domainGeneration: bigint;
  ready: boolean;
}

let syncState: TestSyncState | null = null;
const sent: unknown[] = [];

mock.module("../src/store/sync.ts", () => ({
  currentSyncV2TerminalState: () => syncState,
  requestSyncGenerationRecovery: () => false,
  sendSyncV2Command: (command: unknown) => {
    sent.push(command);
    return syncState?.ready === true;
  },
}));

// The mock must win module resolution before publication captures Sync imports.
const publication = await import("../src/store/terminal-stream-publication.ts");

afterEach(() => {
  terminalDirectRegistry.reset("test cleanup");
  syncState = null;
  sent.length = 0;
});

describe("terminal publication fallback", () => {
  test("uses the ready Sync generation when no direct session route is elected", () => {
    syncState = {
      socketGeneration: 3,
      socketId: "sync-socket",
      processEpoch: "sync-process",
      domainGeneration: 17n,
      ready: true,
    };

    const target = publication.terminalPublicationTarget("session-a");

    expect(target).not.toBeNull();
    expect(target).toMatchObject({
      domainGeneration: 17n,
      transportKind: "sync",
      workerFp: null,
      token: {
        socketGeneration: 3,
        socketId: "sync-socket",
        processEpoch: "sync-process",
        domainGeneration: 17n,
        transportKind: "sync",
        workerFp: null,
      },
    });
    expect(target?.publishView({} as TerminalViewCommand)).toBe(true);
    expect(sent).toHaveLength(1);
  });
});
