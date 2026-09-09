// Headless firehose consumer over the coord's raw WebSocket (/ws/coord-sync).
// The browser SPA (apps/web/src/store/sync.ts), this CLI (`roost api events`),
// and the api_smoke harness all moved OFF the Connect server-streaming `sync`
// RPC: aborting that long-lived streaming response tripped a Bun v1.3.14
// use-after-free in RequestContext.onAbort that crashed the coordinator. A WS
// close routes teardown through Bun's websocket.close callback, never the abort
// path — so the crash is structurally unreachable. This is the Bun-side
// (CLI/test) equivalent of the browser's inline WS.

import { fromBinary } from "@bufbuild/protobuf";
import { FirehoseFrameSchema } from "@roost/shared/proto/sync_pb";
import type { FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { SYNC_WS_PATH } from "@roost/shared/wire/sync-ws";
import { mintJwt } from "../../worker/src/jwt.ts";
import { buildCliContext, CLI_DASHBOARD_HEADER } from "./cli-auth.ts";

export interface SyncWsOptions {
  since?: number;
  signal?: AbortSignal;
}

interface BunWebSocketConstructor {
  new(url: string | URL, options: Bun.WebSocketOptions): WebSocket;
}

const BunWebSocket = WebSocket as unknown as BunWebSocketConstructor;

export function buildHeadlessSyncWsUrl(
  wsBase: string,
  since = 0,
  legacyDashboardId: string | null = null,
): string {
  const url = new URL(wsBase);
  const prefix = url.pathname.replace(/\/+$/, "");
  url.pathname = `${prefix}${SYNC_WS_PATH}`;
  url.search = "";
  url.searchParams.set("since", String(since));
  if (legacyDashboardId) url.searchParams.set("dashboard", legacyDashboardId);
  return url.toString();
}
export function buildHeadlessSyncWsOptions(
  token: string,
  legacyDashboardId: string | null = null,
): Bun.WebSocketOptions {
  const protocols = ["roost-auth", token];
  return legacyDashboardId
    ? { protocols, headers: { [CLI_DASHBOARD_HEADER]: legacyDashboardId } }
    : { protocols };
}

/** Stream FirehoseFrames from the coord WS until the socket closes or `signal`
 *  aborts. Abort → ws.close → clean generator return (no throw), so a caller's
 *  `AbortSignal.timeout(...)` is the normal terminator (drop-in for the old
 *  `for await (const frame of c.sync(...))`). Server→client only; the client
 *  never sends. */
export async function* syncWsFrames(
  wsBase: string,
  token: string,
  opts: SyncWsOptions,
  legacyDashboardId: string | null = null,
): AsyncGenerator<FirehoseFrame> {
  const ws = new BunWebSocket(
    buildHeadlessSyncWsUrl(wsBase, opts.since ?? 0, legacyDashboardId),
    buildHeadlessSyncWsOptions(token, legacyDashboardId),
  );
  ws.binaryType = "arraybuffer";
  const queue: FirehoseFrame[] = [];
  let done = false;
  let wake: (() => void) | null = null;
  const bump = (): void => { const w = wake; wake = null; w?.(); };
  ws.onmessage = (ev): void => {
    try { queue.push(fromBinary(FirehoseFrameSchema, new Uint8Array(ev.data as ArrayBuffer))); }
    catch { /* skip an undecodable frame rather than kill the stream */ }
    bump();
  };
  ws.onclose = (): void => { done = true; bump(); };
  ws.onerror = (): void => { done = true; bump(); };
  const onAbort = (): void => { try { ws.close(); } catch { /* already closing */ } };
  opts.signal?.addEventListener("abort", onAbort, { once: true });
  try {
    while (true) {
      while (queue.length) yield queue.shift()!;
      if (done) return;
      const { promise, resolve } = Promise.withResolvers<void>();
      wake = resolve;
      await promise;
    }
  } finally {
    opts.signal?.removeEventListener("abort", onAbort);
    try { ws.close(); } catch { /* ignore */ }
  }
}

/** Open Sync with the same path-isolated CLI key as the unary RPCs. */
export async function openSyncWs(
  opts: SyncWsOptions = {},
): Promise<AsyncGenerator<FirehoseFrame>> {
  const { cfg, key, legacyDashboardId } = await buildCliContext();
  const token = await mintJwt(key, "roost-coordinator");
  const wsBase = cfg.coordinatorUrl.replace(/^http/, "ws");
  return syncWsFrames(wsBase, token, {
    ...(opts.since !== undefined ? { since: opts.since } : {}),
    ...(opts.signal !== undefined ? { signal: opts.signal } : {}),
  }, legacyDashboardId);
}
