// Multiplexed keeper pool — connection lifecycle + inbound frame dispatch.
// Free functions extracted from MultiplexedKeeperPool (multiplexed-client.ts);
// each takes the pool instance as its first argument. Behavior is unchanged —
// the class methods now delegate here.

import { createConnection } from "node:net";
import { existsSync, mkdirSync, unlinkSync, openSync, closeSync } from "node:fs";
import { join, basename } from "node:path";
import { signal, diag, workerLogDir } from "@roost/shared";
import { MuxFrameType, decodeMuxFrames } from "./protocol-v2.ts";
import { KEEPER_BUILD_STAMP } from "./keeper-stamp.ts";
import { muxSocketPath, MUX_KEEPER_MAIN_TS, BUN_BIN } from "./keeper-pool-config.ts";
import type { MultiplexedKeeperPool } from "./multiplexed-client.ts";

export async function ensureConnection(pool: MultiplexedKeeperPool): Promise<void> {
  if (pool.socket && !pool.socket.destroyed) return;
  if (pool.connectPromise) return pool.connectPromise;
  pool.connectPromise = (async () => {
    const sockPath = muxSocketPath();
    const dir = join(sockPath, "..");
    mkdirSync(dir, { recursive: true });

    // Stale-sock detection: the keeper subprocess writes a sock file at
    // listen() time and unlinks it on clean exit, but a kill -9 / OOM
    // / launchctl bootout leaves the sock file behind. A connect to that
    // file gets ECONNREFUSED. Probe with a quick non-blocking connect; if
    // it fails, unlink the stale sock and respawn.
    if (existsSync(sockPath)) {
      const alive = await new Promise<boolean>((resolve) => {
        const probe = createConnection(sockPath);
        const done = (ok: boolean) => { try { probe.destroy(); } catch { /* ignore */ } resolve(ok); };
        probe.once("connect", () => done(true));
        probe.once("error", () => done(false));
        setTimeout(() => done(false), 500);
      });
      if (!alive) {
        try { unlinkSync(sockPath); } catch { /* ignore */ }
      }
    }

    if (!existsSync(sockPath)) {
      // Discriminator: from source, process.execPath is `bun` and we run the
      // keeper .ts; in the compiled `roost` binary it's the binary itself, so
      // self-exec `roost keeper <sock>` (the .ts isn't on disk). NOT keyed off
      // MUX_KEEPER_MAIN_TS existence — that path derives from import.meta.url,
      // which is synthetic inside a compiled binary.
      const fromSource = basename(process.execPath) === "bun";
      if (fromSource && !existsSync(MUX_KEEPER_MAIN_TS)) {
        throw new Error(`multiplexed-main.ts missing at ${MUX_KEEPER_MAIN_TS}`);
      }
      // Redirect keeper stderr to a real log file. Without this, every
      // console.error / log.error from the keeper subprocess goes to
      // /dev/null (the prior "ignore" stdio) and operators have no way
      // to diagnose a hung session beyond reading the worker's own
      // log. The keeper log lives alongside the worker's at
      // ~/Library/Logs/RoostWorker/keeper.err.log so a `tail -f` in
      // that dir captures everything from the worker process tree.
      // Test spawners set ROOST_KEEPER_QUIET=1 so test keepers don't
      // pollute the production keeper.err.log.
      const quiet = process.env.ROOST_KEEPER_QUIET === "1";
      let keeperStderr: "ignore" | number = "ignore";
      if (!quiet) {
        const logDir = workerLogDir();
        try { mkdirSync(logDir, { recursive: true }); } catch { /* ignore */ }
        const keeperLogPath = join(logDir, "keeper.err.log");
        keeperStderr = openSync(keeperLogPath, "a", 0o644);
      }
      const proc = Bun.spawn({
        cmd: fromSource
          ? [BUN_BIN, "run", MUX_KEEPER_MAIN_TS, sockPath]
          : [process.execPath, "keeper", sockPath],
        detached: true,
        stdio: ["ignore", "ignore", keeperStderr],
        env: process.env as Record<string, string>,
      });
      pool._keeperProc = proc;
      // Fresh keeper = current code by definition.
      pool._runningKeeperStamp = KEEPER_BUILD_STAMP;
      if (typeof keeperStderr === "number") try { closeSync(keeperStderr); } catch { /* parent doesn't need fd; child holds its own */ }
      // Keeper death was previously silent: detached + unref() meant the
      // worker never observed why/when the keeper exited, so a keeper
      // crash left every PTY orphaned with NO log line. unref() detaches
      // the child from the event loop but `.exited` still resolves — await
      // it purely to emit an always-on signal. A non-zero exit or a signal
      // code (SIGKILL/SIGSEGV/OOM) is the smoking gun for "all terminals
      // went 'not connected'". CLAUDE.md keeper-death row.
      void proc.exited.then((exitCode) => {
        signal("keeper.died", {
          exit_code: exitCode,
          signal_code: proc.signalCode ?? null,
          pid: proc.pid,
          sock: sockPath,
        });
      }).catch(() => { /* spawn-time reject already surfaced below */ });
      proc.unref();
      const deadline = Date.now() + 5000;
      while (Date.now() < deadline && !existsSync(sockPath)) {
        await Bun.sleep(50);
      }
      if (!existsSync(sockPath)) {
        pool.connectPromise = null;
        throw new Error(`multiplexed-keeper: socket did not appear at ${sockPath}`);
      }
    }

    await new Promise<void>((resolve, reject) => {
      const s = createConnection(sockPath, () => resolve());
      s.on("data", (chunk: Buffer | Uint8Array) => handleFrameData(pool, Buffer.from(chunk)));
      s.on("close", () => {
        pool.socket = null;
        // Adopted (survivor) keeper: this pool connected to a pre-existing
        // socket without spawning, so no proc.exited above owns its death.
        // Socket close therefore = the adopted keeper died — surface it
        // Tier-1 (the spawn path emits keeper.died via proc.exited instead).
        if (!pool._keeperProc) {
          signal("keeper.died", { reason: "adopted_socket_close", cooldownKey: "keeper" });
        }
        // Clear the resolved connectPromise so a subsequent ensure()
        // re-dials instead of returning the stale resolution.
        // Without this, after a keeper crash every pool.spawn()
        // resolves ensure() instantly then throws "not connected"
        // at the write step → all new sessions fail until the
        // worker process restarts.
        pool.connectPromise = null;
        for (const cbs of pool.channels.values()) cbs.onExit(null);
        pool.channels.clear();
        // Reject in-flight RPCs so callers see "keeper unavailable"
        // immediately instead of hanging until coord's 15s
        // createPendingRpc timeout. Prior shape silently dropped
        // these waiters.
        const closeErr = new Error("multiplexed-keeper: socket closed");
        for (const p of pool.pendingSpawns.values()) p.reject(closeErr);
        pool.pendingSpawns.clear();
        for (const r of pool.pendingListChannels) r.reject(closeErr);
        pool.pendingListChannels = [];
        pool._cachedListChannels = null;
        for (const r of pool.pendingGetHistory) r.reject(closeErr);
        pool.pendingGetHistory = [];
        // Symmetric with the other pending-state clears: avoid leaving
        // a stale promise reference that a future synchronous reader
        // might branch on. The .finally inside listChannels would
        // eventually clear this, but the close handler clears every
        // other field synchronously — keep the same pattern.
        pool._listChannelsInFlight = null;
        // Keeper is gone — drive the worker's resume/respawn reconcile so
        // sessions coord still believes open get fresh PTYs in a new
        // keeper instead of rotting as 'not connected'. Fired AFTER state
        // is cleared so the reconcile's ensure()/listChannels see a clean
        // pool. Deferred a tick so this synchronous close handler returns
        // before the async respawn storm begins.
        if (pool._onKeeperDeath) {
          const fn = pool._onKeeperDeath;
          setTimeout(() => { try { fn(); } catch { /* reconcile logs its own errors */ } }, 0);
        }
      });
      s.on("error", (err) => {
        for (const cbs of pool.channels.values()) cbs.onError(err);
        if (!pool.socket) reject(err);
      });
      pool.socket = s;
    });
  })();
  try {
    await pool.connectPromise;
  } catch (err) {
    // Connection attempt failed — clear so the next ensure() retries.
    pool.connectPromise = null;
    throw err;
  }
  // Declared Promise<void>; don't return the field — close handler can
  // null it synchronously between the await and the return, which
  // would silently violate the signature.
}

function handleFrameData(pool: MultiplexedKeeperPool, chunk: Buffer): void {
  // Buffer.concat already returns a freshly-allocated Buffer; the
  // prior Buffer.from(Buffer.concat(...)) wrapper made a second copy
  // of every PTY chunk. One copy is unavoidable to keep partial-frame
  // state across socket reads; two is wasted CPU on the hot path.
  pool.buf = Buffer.concat([pool.buf, chunk]);
  const { frames, remaining } = decodeMuxFrames(pool.buf);
  pool.buf = remaining;
  for (const f of frames) {
    switch (f.type) {
      case MuxFrameType.SpawnAck: {
        const p = pool.pendingSpawns.get(f.channelId);
        if (p) {
          pool.pendingSpawns.delete(f.channelId);
          let pid = 0;
          try { pid = JSON.parse(f.payload.toString()).pid ?? 0; } catch { /* ignore */ }
          p.resolve(pid);
        }
        break;
      }
      case MuxFrameType.SpawnErr: {
        const p = pool.pendingSpawns.get(f.channelId);
        if (p) {
          pool.pendingSpawns.delete(f.channelId);
          // Drop the pre-registered callback so a stale future
          // Exit/PtyOut frame for a reused channelId doesn't fire
          // the rejected spawn's closures.
          pool.channels.delete(f.channelId);
          let err = "spawn failed";
          try { err = JSON.parse(f.payload.toString()).error ?? err; } catch { /* ignore */ }
          p.reject(new Error(err));
        }
        break;
      }
      case MuxFrameType.PtyOut: {
        const cbs = pool.channels.get(f.channelId);
        if (cbs) cbs.onOutput(f.payload);
        else diag("keeper.pty_out_no_cbs", { channel_id: f.channelId });
        break;
      }
      case MuxFrameType.Exit: {
        const cbs = pool.channels.get(f.channelId);
        let code: number | null = null;
        try { code = JSON.parse(f.payload.toString()).exit_code ?? null; } catch { /* ignore */ }
        if (cbs) {
          cbs.onExit(code);
          pool.channels.delete(f.channelId);
        } else {
          diag("keeper.exit_no_cbs", { channel_id: f.channelId, exit_code: code });
        }
        break;
      }
      case MuxFrameType.ListChannelsResp: {
        let list: Array<{ channelId: number; pid: number }> = [];
        try {
          const raw = JSON.parse(f.payload.toString()) as { channels?: Array<{ channel_id: number; pid: number }> };
          list = (raw.channels ?? []).map(c => ({ channelId: c.channel_id, pid: c.pid }));
        } catch { signal("worker.protocol_violation", { reason: "list_channels_parse_failed", cooldownKey: "keeper" }); }
        // FIFO: each Resp matches the head pending waiter. Prevents
        // the "broadcast to all callers" bug where a single response
        // resolved every pending listChannels() and subsequent
        // responses were silently dropped.
        const waiter = pool.pendingListChannels.shift();
        if (waiter) waiter.resolve(list);
        break;
      }
      case MuxFrameType.GetHistoryResp: {
        // Payload: [8-byte BE head_seq][ring bytes]. Short payload (no
        // 8-byte header) → treat as empty/fresh. FIFO match like
        // ListChannelsResp. Copy the ring out of the shared decode buffer
        // (subarray view) before it's overwritten by the next _onData.
        const waiter = pool.pendingGetHistory.shift();
        if (waiter) {
          if (f.payload.length >= 8) {
            const headSeq = Number(f.payload.readBigUInt64BE(0));
            const bytes = new Uint8Array(f.payload.subarray(8));
            waiter.resolve({ headSeq, bytes });
          } else {
            waiter.resolve({ headSeq: 0, bytes: new Uint8Array(0) });
          }
        }
        break;
      }
      default:
        diag("keeper.unknown_frame", { frame_kind: f.type });
        break;
    }
  }
}
