// Multiplexed keeper pool — connection lifecycle + inbound frame dispatch.
// Connection setup resolves a platform LocalEndpoint and does not expose the
// pool socket until a capability-authenticated Hello completes.

import { existsSync, mkdirSync, openSync, closeSync } from "node:fs";
import { join, basename } from "node:path";
import { signal, diag } from "@roost/shared/diag";
import { localEndpointEnv, prepareLocalEndpoint } from "@roost/shared/local-endpoint";
import { workerLogDir } from "@roost/shared/paths";
import {
  MuxFrameType,
  decodeKeeperHistoryRecords,
  decodeKeeperTerminalState,
  decodeMuxFrames,
  decodePtyInResult,
  decodeResizeResult,
  encodeMuxFrame,
} from "./protocol.ts";
import { KEEPER_BUILD_STAMP } from "./keeper-stamp.ts";
import { muxLocalEndpoint, MUX_KEEPER_MAIN_TS, BUN_BIN } from "./keeper-pool-config.ts";
import { connectKeeperAuthenticated } from "./keeper-probe.ts";
import type { MultiplexedKeeperPool } from "./multiplexed-client.ts";
import {
  settlePendingInput,
  settlePendingKeeperCommandsOnDisconnect,
  settlePendingResize,
  settlePendingTerminalState,
} from "./keeper-pool-io.ts";
import {
  KEEPER_HISTORY_LIVE_BUFFER_MAX_BYTES,
  releasePendingHistoryOutput,
} from "./keeper-pool-channels.ts";

export async function ensureConnection(pool: MultiplexedKeeperPool): Promise<void> {
  if (pool.socket && !pool.socket.destroyed) return;
  if (pool.connectPromise) return pool.connectPromise;
  pool.connectPromise = (async () => {
    const endpoint = muxLocalEndpoint();
    let attempt = await connectKeeperAuthenticated(endpoint);

    if (!attempt.reachable) {
      // POSIX removes a dead socket here; named-pipe preparation is
      // deliberately a no-op because pipe names are not filesystem entries.
      // This happens only after the authenticated adoption dial failed.
      await prepareLocalEndpoint(endpoint);

      // Discriminator: from source, process.execPath is bun/bun.exe and we
      // run the keeper .ts. A compiled roost binary self-execs its keeper
      // subcommand because the source path is synthetic inside the binary.
      const execName = basename(process.execPath).toLowerCase();
      const fromSource = execName === "bun" || execName === "bun.exe";
      if (fromSource && !existsSync(MUX_KEEPER_MAIN_TS)) {
        throw new Error(`multiplexed-main.ts missing at ${MUX_KEEPER_MAIN_TS}`);
      }

      const quiet = process.env.ROOST_KEEPER_QUIET === "1";
      let keeperStderr: "ignore" | number = "ignore";
      if (!quiet) {
        const logDir = workerLogDir();
        try { mkdirSync(logDir, { recursive: true }); } catch { /* ignore */ }
        keeperStderr = openSync(join(logDir, "keeper.err.log"), "a", 0o644);
      }
      const endpointEnv = localEndpointEnv(endpoint, "ROOST_KEEPER");
      const keeperEnv = { ...(process.env as Record<string, string>) };
      for (const name of Object.keys(endpointEnv)) delete keeperEnv[name];
      if (fromSource) Object.assign(keeperEnv, endpointEnv);

      const proc = Bun.spawn({
        cmd: fromSource
          ? [BUN_BIN, "run", MUX_KEEPER_MAIN_TS, endpoint.address]
          : [process.execPath, "keeper", endpoint.address],
        detached: true,
        stdio: ["ignore", "ignore", keeperStderr],
        env: keeperEnv,
      });
      pool._keeperProc = proc;
      pool._runningKeeperStamp = KEEPER_BUILD_STAMP;
      if (typeof keeperStderr === "number") {
        try { closeSync(keeperStderr); } catch { /* child owns its duplicate */ }
      }
      void proc.exited.then((exitCode) => {
        signal("keeper.died", {
          exit_code: exitCode,
          signal_code: proc.signalCode ?? null,
          pid: proc.pid,
          sock: endpoint.address,
        });
      }).catch(() => { /* spawn-time reject is surfaced by readiness below */ });
      proc.unref();

      // Endpoint existence is not readiness (and has no meaning for a named
      // pipe). Keep dialing until a capability-authenticated Hello succeeds.
      const deadline = Date.now() + 5000;
      do {
        attempt = await connectKeeperAuthenticated(endpoint, 500);
        if (attempt.authenticated || attempt.reachable) break;
        await Bun.sleep(50);
      } while (Date.now() < deadline);
    }

    if (!attempt.authenticated) {
      const reason = attempt.reachable
        ? "accepted a connection but did not authenticate"
        : "did not become ready";
      throw new Error(`multiplexed-keeper: endpoint ${reason} at ${endpoint.address}`);
    }
    if (!attempt.compatible) {
      try { attempt.socket.destroy(); } catch { /* already closed */ }
      throw new Error(`multiplexed-keeper: incompatible keeper at ${endpoint.address}`);
    }

    const connection = attempt;
    const s = connection.socket;
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
        for (const waiters of pool.pendingGetHistoryRecords.values()) {
          for (const waiter of waiters) waiter.reject(closeErr);
        }
        pool.pendingGetHistoryRecords.clear();
        pool.pendingHistoryOutput.clear();
        settlePendingKeeperCommandsOnDisconnect(pool);
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
    });
    pool.socket = s;
    pool.setKeeperFeatures(connection.features);
    pool.buf = Buffer.alloc(0);
    for (const frame of connection.pendingFrames) {
      handleFrameData(pool, encodeMuxFrame(frame.type, frame.channelId, frame.payload));
    }
    pool.buf = connection.remaining;
    s.resume();
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
  const dispatchFrom = (start: number): void => {
    for (let frameIndex = start; frameIndex < frames.length; frameIndex++) {
      const f = frames[frameIndex]!;
      let pauseAfterFrame = false;
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
        const historyOutput = pool.pendingHistoryOutput.get(f.channelId);
        if (historyOutput) {
          const retained = Buffer.from(f.payload);
          if (historyOutput.bytes + retained.byteLength <= KEEPER_HISTORY_LIVE_BUFFER_MAX_BYTES) {
            historyOutput.chunks.push(retained);
            historyOutput.bytes += retained.byteLength;
            break;
          }
          const waiters = pool.pendingGetHistoryRecords.get(f.channelId);
          const waiter = waiters?.shift();
          pool.pendingGetHistoryRecords.delete(f.channelId);
          if (waiter) {
            waiter.reject(new Error("getHistoryRecords: live output buffer overflow"));
            pauseAfterFrame = true;
            queueMicrotask(() => {
              releasePendingHistoryOutput(pool, f.channelId);
              pool.channels.get(f.channelId)?.onOutput(retained);
            });
          } else {
            releasePendingHistoryOutput(pool, f.channelId);
            pool.channels.get(f.channelId)?.onOutput(retained);
          }
          break;
        }
        const callbacks = pool.channels.get(f.channelId);
        if (callbacks) callbacks.onOutput(f.payload);
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
      case MuxFrameType.PtyInAck:
      case MuxFrameType.PtyInReject:
      case MuxFrameType.PtyInAmbiguous: {
        const result = decodePtyInResult(f.type, f.payload);
        if (!result) {
          signal("worker.protocol_violation", {
            reason: "keeper_input_result_invalid",
            cooldownKey: "keeper",
          });
          break;
        }
        if (!settlePendingInput(pool, f.channelId, result.inputSeq, result)) {
          diag("keeper.input_result_no_waiter", {
            channel_id: f.channelId,
            input_seq: result.inputSeq,
            result: result.kind,
          });
        }
        break;
      }
      case MuxFrameType.ResizeAck:
      case MuxFrameType.ResizeReject: {
        const result = decodeResizeResult(f.type, f.payload);
        if (!result) {
          signal("worker.protocol_violation", {
            reason: "keeper_resize_result_invalid",
            cooldownKey: "keeper",
          });
          break;
        }
        // The owner's boundary hook runs INSIDE this settle, synchronously,
        // before the loop can dispatch a PtyOut frame that shared this socket
        // read. Deferring the boundary to the promise continuation (one
        // microtask) is what let post-resize bytes reach the old-geometry core.
        if (!settlePendingResize(pool, f.channelId, result.seq, result, "frame")) {
          diag("keeper.resize_result_no_waiter", {
            channel_id: f.channelId,
            resize_seq: result.seq,
            result: result.kind,
          });
        }
        break;
      }
      case MuxFrameType.GetHistoryRecordsResp: {
        const waiters = pool.pendingGetHistoryRecords.get(f.channelId);
        const waiter = waiters?.shift();
        if (waiters?.length === 0) pool.pendingGetHistoryRecords.delete(f.channelId);
        if (!waiter) {
          diag("keeper.history_records_no_waiter", { channel_id: f.channelId });
          break;
        }
        const history = decodeKeeperHistoryRecords(f.payload);
        if (!history) {
          waiter.reject(new Error("getHistoryRecords: invalid keeper response"));
          pauseAfterFrame = true;
          queueMicrotask(() => releasePendingHistoryOutput(pool, f.channelId));
          signal("worker.protocol_violation", {
            reason: "keeper_history_records_invalid",
            cooldownKey: "keeper",
          });
          break;
        }
        pool.pendingHistoryOutput.delete(f.channelId);
        waiter.resolve(history);
        // Apply the retained snapshot before any coalesced live output that
        // follows its exact keeper-stream boundary.
        pauseAfterFrame = true;
        break;
      }
      case MuxFrameType.GetTerminalStateResp: {
        const state = decodeKeeperTerminalState(f.payload);
        if (!state) {
          signal("worker.protocol_violation", {
            reason: "keeper_terminal_state_invalid",
            cooldownKey: "keeper",
          });
        }
        // A malformed payload still settles the head waiter: the recovery path
        // treats null as "authority unreachable" and keeps its floor invalid.
        if (!settlePendingTerminalState(pool, f.channelId, state)) {
          diag("keeper.terminal_state_no_waiter", { channel_id: f.channelId });
        }
        break;
      }
      default:
        diag("keeper.unknown_frame", { frame_kind: f.type });
        break;
      }
      if (pauseAfterFrame && frameIndex + 1 < frames.length) {
        queueMicrotask(() => dispatchFrom(frameIndex + 1));
        return;
      }
    }
  };
  dispatchFrom(0);
}
