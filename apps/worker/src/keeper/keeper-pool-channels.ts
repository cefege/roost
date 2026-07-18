// Multiplexed keeper pool — per-channel spawn, listing, reattach, and history.
// Free functions extracted from MultiplexedKeeperPool (multiplexed-client.ts);
// each takes the pool instance as its first argument. Behavior is unchanged —
// the class methods now delegate here.

import { signal } from "@roost/shared";
import { MuxFrameType, encodeMuxFrame, encodeSpawnRequest } from "./protocol-v2.ts";
import { SPAWN_ACK_TIMEOUT_MS } from "./keeper-pool-config.ts";
import type { MultiplexedKeeperPool, MuxChannelCallbacks } from "./multiplexed-client.ts";

export async function spawnChannel(pool: MultiplexedKeeperPool, opts: {
  channelId: number;
  cwd: string;
  argv: string[];
  cols: number;
  rows: number;
  env?: Record<string, string>;
  callbacks: MuxChannelCallbacks;
}): Promise<number> {
  await pool.ensure();
  if (!pool.socket) throw new Error("multiplexed-keeper: not connected");
  pool.channels.set(opts.channelId, opts.callbacks);
  const pidPromise = new Promise<number>((resolve, reject) => {
    // Ack timeout: a degraded/wedged keeper accepts the Spawn frame but
    // never replies SpawnAck/SpawnErr → the RPC (and the SPA spawn) hangs
    // forever with no trail. Fire spawn.no_ack so `roost doctor` surfaces it
    // (the silent-spawn-hang class from 2026-06-22). resolve/reject clear
    // the timer; the SpawnAck/SpawnErr handlers call them transparently.
    let timer: ReturnType<typeof setTimeout>;
    const entry = {
      resolve: (pid: number) => { clearTimeout(timer); resolve(pid); },
      reject: (e: Error) => { clearTimeout(timer); reject(e); },
    };
    timer = setTimeout(() => {
      if (pool.pendingSpawns.get(opts.channelId) !== entry) return;
      pool.pendingSpawns.delete(opts.channelId);
      pool.channels.delete(opts.channelId);
      signal("spawn.no_ack", {
        channel_id: opts.channelId, cwd: opts.cwd,
        waited_ms: SPAWN_ACK_TIMEOUT_MS, cooldownKey: "keeper",
      });
      entry.reject(new Error(`keeper spawn no-ack after ${SPAWN_ACK_TIMEOUT_MS}ms`));
    }, SPAWN_ACK_TIMEOUT_MS);
    pool.pendingSpawns.set(opts.channelId, entry);
  });
  pool.socket.write(encodeMuxFrame(
    MuxFrameType.Spawn, opts.channelId,
    encodeSpawnRequest({
      channel_id: opts.channelId, cwd: opts.cwd,
      cols: opts.cols, rows: opts.rows,
      argv: opts.argv, env: opts.env,
    }),
  ));
  return pidPromise;
}

export async function listPoolChannels(pool: MultiplexedKeeperPool): Promise<Array<{ channelId: number; pid: number }>> {
  // `!== null` not truthy: an empty array IS a legitimate cache value
  // (worker boots with zero surviving sessions). Truthy-check would
  // miss that — but Array.isArray([]) is true and [] is truthy too,
  // so empty array gates correctly. Use !== null defensively in case
  // a test fixture or future code pre-seeds null.
  if (pool._cachedListChannels !== null) return pool._cachedListChannels;
  if (pool._listChannelsInFlight) return pool._listChannelsInFlight;
  pool._listChannelsInFlight = (async () => {
    await pool.ensure();
    if (!pool.socket) {
      // Transient connection failure during ensure(). Throw instead
      // of returning [] — caching [] here would poison the cache
      // (empty-array is truthy under `if (this._cachedListChannels)`)
      // and freeze SessionManager.resume() into "no surviving
      // sessions" forever, even after the socket reconnects.
      throw new Error("listChannels: keeper socket not connected");
    }
    const result = await new Promise<Array<{ channelId: number; pid: number }>>((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout>;
      const waiter = {
        resolve: (list: Array<{ channelId: number; pid: number }>) => { clearTimeout(timer); resolve(list); },
        reject: (e: Error) => { clearTimeout(timer); reject(e); },
      };
      timer = setTimeout(() => {
        const idx = pool.pendingListChannels.indexOf(waiter);
        if (idx >= 0) pool.pendingListChannels.splice(idx, 1);
        reject(new Error("listChannels timed out after 3000ms"));
      }, 3000);
      pool.pendingListChannels.push(waiter);
      pool.socket!.write(encodeMuxFrame(MuxFrameType.ListChannels, 0, new Uint8Array(0)));
    });
    pool._cachedListChannels = result;
    return result;
  })().finally(() => { pool._listChannelsInFlight = null; });
  return pool._listChannelsInFlight;
}

export async function listPoolChannelsFresh(pool: MultiplexedKeeperPool): Promise<Array<{ channelId: number; pid: number }>> {
  pool._cachedListChannels = null;
  return pool.listChannels();
}

export function reattachChannel(pool: MultiplexedKeeperPool, channelId: number, callbacks: MuxChannelCallbacks): void {
  pool.channels.set(channelId, callbacks);
}

export async function getChannelHistory(pool: MultiplexedKeeperPool, channelId: number): Promise<{ headSeq: number; bytes: Uint8Array }> {
  await pool.ensure();
  if (!pool.socket) throw new Error("getHistory: keeper socket not connected");
  return new Promise<{ headSeq: number; bytes: Uint8Array }>((resolve, reject) => {
    let timer: ReturnType<typeof setTimeout>;
    const waiter = {
      resolve: (r: { headSeq: number; bytes: Uint8Array }) => { clearTimeout(timer); resolve(r); },
      reject: (e: Error) => { clearTimeout(timer); reject(e); },
    };
    timer = setTimeout(() => {
      const idx = pool.pendingGetHistory.indexOf(waiter);
      if (idx >= 0) pool.pendingGetHistory.splice(idx, 1);
      // Old keeper (no GetHistory handler): resolve empty so resume()
      // continues with the pre-RC2 behavior rather than failing the boot.
      resolve({ headSeq: 0, bytes: new Uint8Array(0) });
    }, 3000);
    pool.pendingGetHistory.push(waiter);
    pool.socket!.write(encodeMuxFrame(MuxFrameType.GetHistory, channelId, new Uint8Array(0)));
  });
}
