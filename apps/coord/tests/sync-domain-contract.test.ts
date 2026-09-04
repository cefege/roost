import { expect, test } from "bun:test";
import { SyncDomain } from "@roost/shared/proto/sync_pb";
import {
  V2_DOMAINS,
  createSyncV2SocketState,
  isLazyDomain,
} from "../src/connect/sync-ws-v2-state.ts";

const SURVIVING_DOMAINS = [
  SyncDomain.TERMINAL,
  SyncDomain.WORKERS,
  SyncDomain.WORKSPACES,
  SyncDomain.TASKS,
  SyncDomain.MCP,
  SyncDomain.PAIR,
  SyncDomain.AUDIT,
] as const;

test("Sync advertises exactly the seven surviving stable domain IDs", () => {
  expect([...V2_DOMAINS]).toEqual([...SURVIVING_DOMAINS]);
  expect([...V2_DOMAINS]).toEqual([1, 2, 3, 4, 6, 7, 9]);
});

test("audit is the only lazy Sync domain in a fresh generation snapshot", () => {
  const state = createSyncV2SocketState();
  expect([...state.domains.keys()]).toEqual([...SURVIVING_DOMAINS]);
  for (const domain of SURVIVING_DOMAINS) {
    expect(state.domains.get(domain)).toMatchObject({
      ready: false,
      subscribed: domain !== SyncDomain.AUDIT,
    });
  }
  expect(V2_DOMAINS.filter(isLazyDomain)).toEqual([SyncDomain.AUDIT]);
});
