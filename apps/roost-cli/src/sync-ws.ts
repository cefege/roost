// Headless firehose consumer over the coord's raw WebSocket (/ws/coord-sync).
// The browser SPA (apps/web/src/store/sync.ts), this CLI (`roost api events`),
// and the api_smoke harness all moved OFF the Connect server-streaming `sync`
// RPC: aborting that long-lived streaming response tripped a Bun v1.3.14
// use-after-free in RequestContext.onAbort that crashed the coordinator. A WS
// close routes teardown through Bun's websocket.close callback, never the abort
// path — so the crash is structurally unreachable. This is the Bun-side
// (CLI/test) equivalent of the browser's inline WS.

import { fromBinary } from "@bufbuild/protobuf";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { FirehoseFrameSchema, type FirehoseFrame } from "@roost/shared/proto/sync_pb";
import { loadWorkerConfig } from "../../worker/src/config.ts";
import { loadWorkerKey, mintJwt } from "../../worker/src/jwt.ts";

/** Stream FirehoseFrames from the coord WS until the socket closes or `signal`
 *  aborts. Abort → ws.close → clean generator return (no throw), so a caller's
 *  `AbortSignal.timeout(...)` is the normal terminator (drop-in for the old
 *  `for await (const frame of c.sync(...))`). Server→client only; the client
 *  never sends. */
export async function* syncWsFrames(
  wsBase: string,
  token: string,
  opts: { since?: number; signal?: AbortSignal } = {},
): AsyncGenerator<FirehoseFrame> {
  const url = `${wsBase}/ws/coord-sync?since=${opts.since ?? 0}`;
  const ws = new WebSocket(url, ["roost-auth", token]);
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

/** Open the firehose WS using the SAME coord URL + headless key as
 *  buildApiClient (worker key, else ~/.roost/cli-key; ROOST_COORD_URL override).
 *  The key must already be in coord's authorized_keys (same prerequisite as the
 *  Connect client). */
export async function openSyncWs(
  opts: { since?: number; signal?: AbortSignal } = {},
): Promise<AsyncGenerator<FirehoseFrame>> {
  const cfg = loadWorkerConfig();
  if (process.env.ROOST_COORD_URL) cfg.coordinatorUrl = process.env.ROOST_COORD_URL;
  const keyPath = existsSync(cfg.workerKeyPath) ? cfg.workerKeyPath : join(homedir(), ".roost", "cli-key");
  const key = await loadWorkerKey(keyPath);
  const token = await mintJwt(key, "roost-coordinator");
  const wsBase = cfg.coordinatorUrl.replace(/^http/, "ws");
  return syncWsFrames(wsBase, token, opts);
}
