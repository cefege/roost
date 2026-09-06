// Pins explicit scrollback-search cancellation before worker admission.
// The bounded tombstone closes cross-request arrival races without retaining
// unbounded browser-provided identities in the worker session manager.

import { describe, expect, test } from "bun:test";
import { asSessionId, asWorkerFp } from "@roost/shared/wire";
import type { ClientControlFrame } from "@roost/shared/wire";
import { SessionManager } from "../src/session-manager.ts";
import {
  cancelSearchScrollback,
  handleSearchScrollback,
} from "../src/terminal-search.ts";
import type { CoordLink } from "../src/transport/coord-link-types.ts";
import { LifecycleTestSink } from "./lifecycle-test-sink.ts";

const SESSION_ID = asSessionId("00000000-0000-0000-0000-000000000001");

function manager(): SessionManager {
  return new SessionManager({
    workerFp: asWorkerFp("00".repeat(32)),
    sink: new LifecycleTestSink(),
  });
}

function searchFrame(searchId: string): Extract<ClientControlFrame, { kind: "search-scrollback" }> {
  return {
    kind: "search-scrollback",
    request_id: "inner-request",
    session_id: SESSION_ID,
    search_id: searchId,
    grid_epoch: "",
    query: "needle",
    case_sensitive: false,
    regex: false,
    max_rows: 100,
    max_matches: 20,
  };
}

describe("scrollback search cancellation admission", () => {
  test("cancel arriving first rejects the later matching search", async () => {
    const sessionManager = manager();
    cancelSearchScrollback({
      kind: "cancel-scrollback-search",
      request_id: "cancel",
      session_id: SESSION_ID,
      search_request_id: "search-id",
    }, "viewer-a", sessionManager);
    const sent: Array<{ kind: string; request_id: string; message?: string }> = [];
    const coordLink = {
      send: (frame: { kind: string; request_id: string; message?: string }) => {
        sent.push(frame);
        return true;
      },
    } as unknown as CoordLink;

    await handleSearchScrollback(searchFrame("search-id"), "outer-request", {
      coordLink,
      sessionMgr: sessionManager,
      searchOwnerId: "viewer-a",
    });

    expect(sent).toEqual([{
      kind: "rpc-error",
      request_id: "outer-request",
      message: "scrollback search superseded",
    }]);
    expect(sessionManager.terminalSearchCancellations.size).toBe(0);
  });

  test("the oldest tombstone survives at exact capacity before bounded eviction", async () => {
    const sessionManager = manager();
    for (let index = 0; index < 128; index++) {
      cancelSearchScrollback({
        kind: "cancel-scrollback-search",
        request_id: `cancel-${index}`,
        session_id: SESSION_ID,
        search_request_id: `search-${index}`,
      }, "viewer-a", sessionManager);
    }
    expect(sessionManager.terminalSearchCancellations.size).toBe(128);
    const sent: Array<{ kind: string }> = [];
    const coordLink = {
      send: (frame: { kind: string }) => { sent.push(frame); return true; },
    } as unknown as CoordLink;
    await handleSearchScrollback(searchFrame("search-0"), "oldest", {
      coordLink, sessionMgr: sessionManager, searchOwnerId: "viewer-a",
    });
    expect(sent).toHaveLength(1);

    for (let index = 128; index < 256; index++) {
      cancelSearchScrollback({
        kind: "cancel-scrollback-search",
        request_id: `cancel-${index}`,
        session_id: SESSION_ID,
        search_request_id: `search-${index}`,
      }, "viewer-a", sessionManager);
    }
    expect(sessionManager.terminalSearchCancellations.size).toBeLessThanOrEqual(128);
  });

  test("channel teardown aborts and removes every viewer search", () => {
    const sessionManager = manager();
    const first = new AbortController();
    const second = new AbortController();
    sessionManager.terminalSearches.set("7:viewer-a", { searchId: "a", controller: first });
    sessionManager.terminalSearches.set("7:viewer-b", { searchId: "b", controller: second });
    sessionManager._dropChannelState(7);
    expect(first.signal.aborted).toBe(true);
    expect(second.signal.aborted).toBe(true);
    expect(sessionManager.terminalSearches.size).toBe(0);
  });
});
