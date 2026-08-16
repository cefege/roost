// Worker-side client for the multiplexed keeper. ONE socket + ONE
// keeper subprocess (multiplexed-main.ts) hosts N channels;
// channel_id discriminates per frame. Only mode — the per-session keeper
// + ROOST_KEEPER_MODE switch were retired 2026-06-15.
//
// Surface: MultiplexedKeeperPool.ensure() starts the keeper (idempotent);
// pool.spawn({channelId, shellSpec, cols, rows}) opens a channel;
// pool.attach(channelId, callbacks) registers per-channel handlers;
// pool.input/resize/kill drive the channel from worker side.
//
// Callers: session-manager.ts when MUX mode is on.

import { type Socket } from "node:net";
import { signal } from "@roost/shared";
import { ensureConnection } from "./keeper-pool-lifecycle.ts";
import {
  spawnChannel,
  listPoolChannels,
  listPoolChannelsFresh,
  reattachChannel,
  getChannelHistory,
  getChannelHistoryRecords,
} from "./keeper-pool-channels.ts";
import {
  channelInput,
  channelInputRequest,
  channelResize,
  channelResizeRequest,
  channelResizeStatus,
  channelKill,
  disposePool,
} from "./keeper-pool-io.ts";
import type {
  KeeperHistoryRecords,
  KeeperInputResult,
  KeeperResizeResult,
} from "./protocol-v2.ts";
import type { ShellSpec } from "../shell-spec.ts";

export { probeKeeperCompatible, shutdownKeeperAuthenticated } from "./keeper-probe.ts";
export type {
  KeeperHistoryRecord,
  KeeperHistoryRecords,
  KeeperInputResult,
  KeeperResizeResult,
} from "./protocol-v2.ts";

export type MuxChannelCallbacks = {
  onOutput: (chunk: Buffer) => void;
  onExit: (exitCode: number | null) => void;
  onError: (err: Error) => void;
};

export class MultiplexedKeeperPool {
  socket: Socket | null = null;
  buf: Buffer = Buffer.alloc(0) as Buffer;
  channels = new Map<number, MuxChannelCallbacks>();
  pendingSpawns = new Map<number, { resolve: (pid: number) => void; reject: (e: Error) => void }>();
  // FIFO of waiters. Each ListChannelsResp frame resolves exactly the
  // head — preserves request/response 1:1 ordering across concurrent
  // listChannels() callers. (We send one ListChannels frame per
  // pending request; the keeper replies in arrival order.) Carries
  // both halves so the close handler can REJECT in-flight waiters —
  // resolving them with [] would let the IIFE cache the empty array
  // and serve it forever via the truthy `_cachedListChannels` gate.
  pendingListChannels: Array<{
    resolve: (list: Array<{ channelId: number; pid: number }>) => void;
    reject: (e: Error) => void;
  }> = [];
  _cachedListChannels: Array<{ channelId: number; pid: number }> | null = null;
  _listChannelsInFlight: Promise<Array<{ channelId: number; pid: number }>> | null = null;
  // RC2: FIFO waiters for getHistory(). Keyed FIFO like pendingListChannels
  // (keeper replies in arrival order). NOT cached — head_seq advances on
  // every PTY chunk, so each call must hit the keeper fresh.
  pendingGetHistory: Array<{
    resolve: (r: { headSeq: number; bytes: Uint8Array }) => void;
    reject: (e: Error) => void;
  }> = [];
  pendingGetHistoryRecords = new Map<number, Array<{
    resolve: (history: KeeperHistoryRecords) => void;
    reject: (error: Error) => void;
  }>>();
  pendingHistoryOutput = new Map<number, { chunks: Buffer[]; bytes: number }>();
  pendingInputs = new Map<string, {
    channelId: number;
    inputSeq: number;
    expectedBytes: number;
    timer: ReturnType<typeof setTimeout>;
    resolve: (result: KeeperInputResult) => void;
  }>();
  pendingResizes = new Map<string, {
    channelId: number;
    seq: number;
    timer: ReturnType<typeof setTimeout>;
    resolve: (result: KeeperResizeResult) => void;
  }>();
  _pendingInputUsage = new Map<number, { commands: number; bytes: number }>();
  connectPromise: Promise<void> | null = null;
  // Invoked once per worker→keeper socket close (= keeper died). main.ts
  // registers the boot resume/respawn reconcile loop here so a keeper that
  // dies WHILE THE WORKER STAYS ALIVE recovers sessions the same way a
  // worker restart does — without it, the loop only ran at boot and a
  // mid-life keeper death left every PTY a 'not connected' zombie.
  _onKeeperDeath: (() => void) | null = null;
  setOnKeeperDeath(fn: () => void): void { this._onKeeperDeath = fn; }
  // Handle to the live keeper subprocess so a DEGRADED survivor (births dead
  // PTYs without dying — emit_no_session bursts) can be force-restarted. Kill
  // → socket close → _onKeeperDeath → reconcile → ensure() spawns a fresh one.
  _keeperProc: Bun.Subprocess | null = null;

  // KEEPER_BUILD_STAMP the RUNNING keeper reports. Set to our own stamp when
  // we spawn a fresh keeper (= current code); set by the worker to a survivor's
  // reported stamp when it adopts one at boot (may be older code). Heartbeat
  // reads it to flag a stale keeper. null until the first keeper is known.
  _runningKeeperStamp: string | null = null;
  getRunningKeeperStamp(): string | null { return this._runningKeeperStamp; }
  /** Record the stamp of a keeper the worker ADOPTED (compatible survivor at
   *  boot). Fresh spawns set the stamp themselves in ensure(). */
  setRunningKeeperStamp(stamp: string): void { this._runningKeeperStamp = stamp; }
  /** Features returned by the authenticated Hello for this exact socket. */
  keeperFeatures = new Set<string>();
  setKeeperFeatures(features: readonly string[]): void {
    this.keeperFeatures = new Set(features);
  }
  supportsKeeperFeature(feature: string): boolean {
    return this.keeperFeatures.has(feature);
  }

  /** Force a fresh keeper: kill the current subprocess. The socket-close
   *  handler (above) clears pool state + drives the worker's reconcile, and
   *  the reconcile's respawn → ensure() starts a clean keeper. No-op if no
   *  keeper is running (next ensure() spawns one anyway). */
  restartKeeper(): void {
    const proc = this._keeperProc;
    if (!proc) return;
    signal("keeper.restart_degraded", { pid: proc.pid });
    try { proc.kill(); } catch { /* already dead → close handler ran */ }
  }

  /** Start the multiplexed keeper subprocess (idempotent) and open the
   * worker→keeper socket. Polls up to 5s for the socket file. */
  async ensure(): Promise<void> {
    return ensureConnection(this);
  }

  /** Open a new PTY channel. Registers callbacks BEFORE sending Spawn so
   * the first PtyOut frames after SpawnAck route correctly. */
  async spawn(opts: {
    channelId: number;
    shellSpec: ShellSpec;
    cols: number;
    rows: number;
    callbacks: MuxChannelCallbacks;
  }): Promise<number> {
    return spawnChannel(this, opts);
  }

  /** Cross-process resume — query the keeper for its live channel set
   *  so a fresh worker can re-register output callbacks against PTYs
   *  spawned by a prior worker instance.
   *
   *  Memoized for the lifetime of this pool — channel set is static
   *  within a single worker boot. Caller is `SessionManager.resume()`
   *  which fires once per surviving session at startup. Re-call after
   *  reset() if you need a fresh view (no current call site needs it). */
  async listChannels(): Promise<Array<{ channelId: number; pid: number }>> {
    return listPoolChannels(this);
  }

  /** Force a fresh ListChannels round-trip, bypassing the per-keeper-life
   *  cache. Plain listChannels() caches a reconcile-time snapshot cleared only
   *  on keeper death (line 281) — correct for advanceChannelCounterPastKeeper /
   *  resume() which run right after a death. The reverse-reap sweep
   *  (SessionManager.reapStrayKeeperChannels) polls repeatedly during a single
   *  keeper's life, so it needs the CURRENT set each tick: else a just-spawned
   *  channel stays invisible (falsely a stray) and a reaped one never drops out. */
  async listChannelsFresh(): Promise<Array<{ channelId: number; pid: number }>> {
    return listPoolChannelsFresh(this);
  }

  /** Re-register channel callbacks WITHOUT sending Spawn (the PTY is
   *  already running in the keeper from a prior worker). Used by
   *  SessionManager.resume() in mux mode. */
  reattach(channelId: number, callbacks: MuxChannelCallbacks): void {
    reattachChannel(this, channelId, callbacks);
  }

  /** RC2 cross-process history resume — fetch a surviving channel's
   *  retained output ring + head_seq so SessionManager.resume() can
   *  re-seed scrollback + head_seq instead of zeroing. Resolves
   *  {headSeq:0, bytes:[]} on a keeper that pre-dates GetHistory (the
   *  GetHistoryResp never arrives → timeout → caller falls back to the
   *  old empty-ring behavior). NOT cached — head_seq advances per chunk. */
  async getHistory(channelId: number): Promise<{ headSeq: number; bytes: Uint8Array }> {
    return getChannelHistory(this, channelId);
  }

  /** Capability-gated ordered output/resize history for worker adoption. */
  async getHistoryRecords(channelId: number): Promise<KeeperHistoryRecords> {
    return getChannelHistoryRecords(this, channelId);
  }

  // Per-(logTarget) timestamp of the last "dropped: socket closed"
  // warn, so a drag-resize burst (~60 fps) or a 5 KB paste during the
  // ms-to-seconds reconnect window emits ONE log line per second per
  // target instead of one per frame. The dropped-frame count between
  // logs is reported in the next emission.
  _dropLogState: Map<string, { lastTs: number; suppressed: number }> = new Map();

  input(channelId: number, bytes: Uint8Array): void {
    channelInput(this, channelId, bytes);
  }

  /** Acknowledged logical input. Full ACK is the only accepted result. */
  requestInput(channelId: number, inputSeq: number, bytes: Uint8Array): Promise<KeeperInputResult> {
    return channelInputRequest(this, channelId, inputSeq, bytes);
  }

  resize(channelId: number, cols: number, rows: number): void {
    channelResize(this, channelId, cols, rows);
  }

  /** Apply a logical resize sequence at most once. */
  requestResize(channelId: number, seq: number, cols: number, rows: number): Promise<KeeperResizeResult> {
    return channelResizeRequest(this, channelId, seq, cols, rows);
  }

  /** Query the keeper's cached result without reapplying terminal.resize. */
  queryResizeStatus(channelId: number, seq: number): Promise<KeeperResizeResult> {
    return channelResizeStatus(this, channelId, seq);
  }

  kill(channelId: number): void {
    channelKill(this, channelId);
  }

  /** Force-tear pool (used in tests; production has no shutdown path —
   * the keeper survives worker restart, same as legacy per-session). */
  dispose(): void {
    disposePool(this);
  }
}

/** Module-level singleton. SessionManager wires every spawn through
 *  this; the legacy per-session keeper was retired 2026-06-15. */
let _pool: MultiplexedKeeperPool | null = null;
export function getMultiplexedPool(): MultiplexedKeeperPool {
  if (!_pool) _pool = new MultiplexedKeeperPool();
  return _pool;
}
