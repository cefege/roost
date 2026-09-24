// Direct history selection tests isolate route election from renderer paging.
// They prove history uses only the exact elected route and coordinator fallback
// happens once only after a direct request explicitly fails or exceeds its limit.
// Input and cell carriers are absent from this history-only dependency seam.

import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import type { TerminalGenerationToken } from "../src/store/terminal-stream-types.ts";
import type { PbCellRow } from "@roost/protocol/proto/cell_pb";

type ScrollbackRequest = {
  sessionId: string;
  endRow: bigint;
  maxRows: number;
  gridEpoch: string;
};
type DirectPage = {
  error: string;
  rows: PbCellRow[];
  cols: number;
  scrollbackTotal: bigint;
  startRow: bigint;
  endRow: bigint;
  gridEpoch: string;
  historyFloor: number;
};
interface DirectHistoryConnection {
  token(): TerminalGenerationToken | null;
  requestScrollback(query: ScrollbackRequest): Promise<DirectPage>;
}

const SESSION_ID = "session-direct-history";
const QUERY: ScrollbackRequest = {
  sessionId: SESSION_ID,
  endRow: 250n,
  maxRows: 250,
  gridEpoch: "grid-direct-history",
};
const DIRECT_TOKEN: TerminalGenerationToken = {
  socketGeneration: 9,
  socketId: "direct-history-socket",
  processEpoch: "direct-history-worker",
  domainGeneration: 4n,
  transportKind: "loopback",
  workerFp: "direct-history-worker-fp",
};

const coordinatorCalls: ScrollbackRequest[] = [];
const directCalls: ScrollbackRequest[] = [];
let coordinatorRead: (query: ScrollbackRequest) => Promise<DirectPage>;
let directConnection: DirectHistoryConnection | null = null;
let currentToken: TerminalGenerationToken | null = null;

mock.module("../src/client/rpc/connect.ts", () => ({
  coordClient: {
    sessionsGetScrollbackCells(query: ScrollbackRequest) {
      coordinatorCalls.push(query);
      return coordinatorRead(query);
    },
  },
}));
mock.module("../src/store/terminal-stream-transport.ts", () => ({
  terminalDirectRegistry: {
    activeForSession(sessionId: string) {
      return sessionId === SESSION_ID ? directConnection : null;
    },
  },
}));
mock.module("../src/store/terminal-stream-publication.ts", () => ({
  currentTerminalGenerationToken(sessionId: string) {
    return sessionId === SESSION_ID ? currentToken : null;
  },
}));

// Must follow mock.module so the resolver binds its transport seams to this fixture.
const { requestScrollbackPage } = await import("../src/lib/scrollbackDirectHistory.ts");

function page(error = ""): DirectPage {
  return {
    error,
    rows: [],
    cols: 80,
    scrollbackTotal: 250n,
    startRow: 0n,
    endRow: 250n,
    gridEpoch: QUERY.gridEpoch,
    historyFloor: 0,
  };
}

function direct(read: (query: ScrollbackRequest) => Promise<DirectPage>): DirectHistoryConnection {
  return {
    token: () => DIRECT_TOKEN,
    requestScrollback(query) {
      directCalls.push(query);
      return read(query);
    },
  };
}

beforeEach(() => {
  coordinatorCalls.length = 0;
  directCalls.length = 0;
  directConnection = null;
  currentToken = null;
  coordinatorRead = async () => page();
});
afterEach(() => {
  directConnection = null;
  currentToken = null;
});

describe("requestScrollbackPage", () => {
  test("uses the exact elected direct connection before coordinator history", async () => {
    const directPage = page();
    currentToken = { ...DIRECT_TOKEN };
    directConnection = direct(async () => directPage);

    expect(await requestScrollbackPage(SESSION_ID, QUERY)).toBe(directPage);
    expect(directCalls).toEqual([QUERY]);
    expect(coordinatorCalls).toEqual([]);
  });

  test("rejects a route whose current generation token differs", async () => {
    currentToken = { ...DIRECT_TOKEN, socketId: "stale-direct-history-socket" };
    directConnection = direct(async () => page());

    await requestScrollbackPage(SESSION_ID, QUERY);

    expect(directCalls).toEqual([]);
    expect(coordinatorCalls).toEqual([QUERY]);
  });

  test("falls back once when the direct history request fails", async () => {
    currentToken = { ...DIRECT_TOKEN };
    directConnection = direct(async () => {
      throw new Error("direct history socket closed");
    });

    await requestScrollbackPage(SESSION_ID, QUERY);

    expect(directCalls).toEqual([QUERY]);
    expect(coordinatorCalls).toEqual([QUERY]);
  });

  test("falls back once when direct history exceeds its transport limit", async () => {
    currentToken = { ...DIRECT_TOKEN };
    directConnection = direct(async () => page("scrollback response exceeds direct transport limit"));

    await requestScrollbackPage(SESSION_ID, QUERY);

    expect(directCalls).toEqual([QUERY]);
    expect(coordinatorCalls).toEqual([QUERY]);
  });

  test("does not mask a direct history rejection as coordinator fallback", async () => {
    currentToken = { ...DIRECT_TOKEN };
    directConnection = direct(async () => page("direct reader unavailable"));

    await expect(requestScrollbackPage(SESSION_ID, QUERY)).rejects.toThrow("direct terminal scrollback request was rejected");

    expect(directCalls).toEqual([QUERY]);
    expect(coordinatorCalls).toEqual([]);
  });
});
