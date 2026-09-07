// Registers the mocked coord scrollback-search RPC for the terminal-find
// suites and owns the request/signal/cancellation spies they assert on.
// Imported before ../src/lib/terminalFindController.ts by
// terminalFindController-test-harness.ts, so the controller binds the mock;
// depends only on bun:test and the generated coordinator types.

import { mock } from "bun:test";
import type { ScrollbackHistoryFloor, SearchStopReason } from "@roost/shared/proto/coordinator_pb";

export interface SearchRequest {
  sessionId: string; searchId: string; gridEpoch: string; query: string;
  caseSensitive: boolean; regex: boolean; beforeRow?: bigint;
  maxRows: number; maxMatches: number;
}

export interface SearchResponse {
  matches: Array<{ row: bigint; col: number; len: number; preview: string }>;
  truncated: boolean; scrollbackTotal: bigint; cols: number; gridEpoch: string;
  scannedStartRow: bigint; scannedEndRow: bigint;
  historyFloor: ScrollbackHistoryFloor; nextBeforeRow?: bigint;
  stopReason: SearchStopReason;
}

export interface SearchCallOptions { signal?: AbortSignal }

export type SearchRpc = (
  request: SearchRequest,
  options?: SearchCallOptions,
) => Promise<SearchResponse>;

export const requests: SearchRequest[] = [];
export const signals: AbortSignal[] = [];
export const cancellationRequests: Array<{ sessionId: string; searchId: string }> = [];

let rpcImpl: SearchRpc = async () => {
  throw new Error("terminal-find search RPC used before setSearchRpc");
};

export function setSearchRpc(impl: SearchRpc): void {
  rpcImpl = impl;
}

mock.module("../src/connect.ts", () => ({
  coordClient: {
    sessionsSearchScrollback(request: SearchRequest, options?: SearchCallOptions) {
      requests.push(request);
      if (options?.signal) signals.push(options.signal);
      return rpcImpl(request, options);
    },
    async sessionsCancelScrollbackSearch(request: { sessionId: string; searchId: string }) {
      cancellationRequests.push(request);
      return {};
    },
  },
}));
