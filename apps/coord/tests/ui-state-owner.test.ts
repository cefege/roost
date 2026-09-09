// Pins coordinator-owned UI report TTL, identity admission, and cardinality bounds.
// Focused instances use an injected clock and disabled interval so heartbeat and
// new-tab behavior are deterministic without a database or WebSocket fixture.
// The UI bus assertion prevents canonical documents from gaining replay retention.

import { describe, expect, test } from "bun:test";
import { create } from "@bufbuild/protobuf";
import {
  UiReportStateRequestSchema,
  type UiReportStateRequest,
} from "@roost/shared/proto/sync_pb";
import { uiBus } from "../src/buses.ts";
import {
  UI_STATE_TTL_MS,
  UiStateCapacityError,
  UiStateIdentityRateError,
  UiStateOwner,
} from "../src/connect/ui-state-owner.ts";

function state(tabId: string, activePath = "/"): UiReportStateRequest {
  return create(UiReportStateRequestSchema, { tabId, activePath, folderKey: "folder" });
}

function report(
  owner: UiStateOwner,
  fingerprint: string,
  tabId: string,
  activePath = "/",
): void {
  owner.report({
    fingerprint,
    tabId,
    state: state(tabId, activePath),
  });
}

describe("UiStateOwner identity admission", () => {
  test("rate limits only new identities while existing heartbeats remain admitted", () => {
    let now = 0;
    const owner = new UiStateOwner({
      now: () => now,
      reapIntervalMs: null,
      maxTabsPerFingerprint: 4,
      maxTabsTotal: 8,
      newIdentitiesPerWindow: 2,
      identityWindowMs: 100,
    });
    report(owner, "fingerprint", "tab-a");
    now = 1;
    report(owner, "fingerprint", "tab-b");
    now = 2;
    report(owner, "fingerprint", "tab-a", "/heartbeat");
    expect(owner.list().find((entry) => entry.tabId === "tab-a")?.state.activePath)
      .toBe("/heartbeat");
    expect(() => report(owner, "fingerprint", "tab-c"))
      .toThrow(UiStateIdentityRateError);

    now = 100;
    report(owner, "fingerprint", "tab-c");
    expect(owner.snapshot().map((entry) => entry.tabId))
      .toEqual(["tab-a", "tab-b", "tab-c"]);
    owner.dispose();
  });

  test("fails deterministic per-fingerprint and aggregate caps without eviction", () => {
    let now = 0;
    const owner = new UiStateOwner({
      now: () => now,
      reapIntervalMs: null,
      maxTabsPerFingerprint: 2,
      maxTabsTotal: 3,
      newIdentitiesPerWindow: 20,
    });
    report(owner, "fingerprint-a", "tab-a1");
    now = 1;
    report(owner, "fingerprint-a", "tab-a2");
    now = 2;
    report(owner, "fingerprint-b", "tab-b1");
    expect(() => report(owner, "fingerprint-a", "tab-a3"))
      .toThrow(UiStateCapacityError);
    expect(() => report(owner, "fingerprint-b", "tab-b2"))
      .toThrow(UiStateCapacityError);
    expect(owner.snapshot().map((entry) => entry.tabId))
      .toEqual(["tab-a1", "tab-a2", "tab-b1"]);

    now = 3;
    report(owner, "fingerprint-b", "tab-b1", "/heartbeat");
    now = UI_STATE_TTL_MS + 2;
    report(owner, "fingerprint-b", "tab-b2");
    expect(owner.snapshot().map((entry) => entry.tabId))
      .toEqual(["tab-b1", "tab-b2"]);
    owner.dispose();
  });

  test("capacity failures do not charge a later successful identity admission", () => {
    let now = 0;
    const owner = new UiStateOwner({
      now: () => now,
      reapIntervalMs: null,
      maxTabsPerFingerprint: 2,
      maxTabsTotal: 1,
      newIdentitiesPerWindow: 1,
      identityWindowMs: UI_STATE_TTL_MS * 2,
    });
    report(owner, "fingerprint-a", "tab-a");
    expect(() => report(owner, "fingerprint-b", "tab-b"))
      .toThrow(UiStateCapacityError);

    now = UI_STATE_TTL_MS + 1;
    report(owner, "fingerprint-b", "tab-b");
    expect(owner.snapshot().map((entry) => entry.tabId)).toEqual(["tab-b"]);
    owner.dispose();
  });
});

test("UI bus retains no report or command payloads", () => {
  uiBus.publish({
    kind: "state",
    fp: "fingerprint",
    tabId: "tab",
    state: state("tab"),
  });
  expect(uiBus.retainedCount).toBe(0);
});
