// Mux frame dispatch for the multiplexed keeper. The entry owns the single
// channels Map + broadcast function; shell policy arrives fully resolved in
// SpawnRequest.shell_spec.

import {
  KEEPER_MAX_HISTORY_RESIZE_RECORDS,
  KEEPER_MAX_INPUT_BYTES,
  KEEPER_MAX_TERMINAL_DIMENSION,
  KEEPER_PROTOCOL_VERSION,
  MuxFrameType,
  decodePtyInRequest,
  decodeResizeRequest,
  decodeResizeStatusQuery,
  encodeKeeperHistoryRecords,
  encodeMuxFrame,
  encodePtyInResult,
  encodeResizeResult,
  isEmptyKeeperPayload,
  decodeSpawnRequest,
} from "./protocol-v2.ts";
import type {
  KeeperHistoryRecord,
  KeeperHistoryRecords,
  PtyInWireResult,
  ResizeWireResult,
} from "./protocol-v2.ts";
import { KEEPER_BUILD_STAMP } from "./keeper-stamp.ts";
import { _log, _keeperOpenFdCount } from "./keeper-log.ts";
import { reapChannelTree } from "./keeper-process-reap.ts";
import type { Channel, ClientState } from "./keeper-types.ts";
import { createSbRing, appendToRing, readRing, ringLength } from "../session-scrollback-ring.ts";
import { assertNeverPlatform, supportedHostPlatform } from "@roost/shared/platform";
import { nativePathToFsPath } from "@roost/shared/native-path";
import { spawnWindowsJobHost } from "@roost/shared/windows-helper";
import type { WindowsJobHostHandle } from "@roost/shared/windows-helper";

// RC2: per-channel output ring kept on the keeper so head_seq + history
// survive a worker restart (the keeper outlives the worker). Matches the
// worker-side SCROLLBACK_CAP_BYTES so a resume re-seeds the same depth.
// 2026-06-22: 8 MB → 1 MB. The keeper is the jetsam victim on a permanently
// RAM-full box (detached child, low jetsam band); 12 channels × 8 MB = 96 MB
// worst-case made it a fat target. 1 MB/ch (~10k lines) keeps generous depth
// while cutting the worst-case footprint to 12 MB. See memory
// project_keeper_death_auto_respawn (jetsam root cause).
const KEEPER_RING_CAP_BYTES = 1 * 1024 * 1024;

// GetHistory on an unknown/exited channel: no ring to read.
const EMPTY_U8 = new Uint8Array(0);
const SPAWNING_CHANNELS = new Set<number>();
const KEEPER_INPUT_DEADLINE_MS = 2000;
const KEEPER_INPUT_QUEUE_MAX_COMMANDS = 200;
const KEEPER_INPUT_QUEUE_MAX_BYTES = 256 * 1024;
const RESIZE_STATUS_CACHE_MAX = KEEPER_MAX_HISTORY_RESIZE_RECORDS;

function trimEvictedResizeHistory(ch: Channel): void {
  const retainedTail = ch.headSeq - ringLength(ch.outRing);
  let removeCount = 0;
  while (removeCount < ch.historyResizes.length
      && ch.historyResizes[removeCount]!.headSeq <= retainedTail) {
    const evicted = ch.historyResizes[removeCount]!;
    ch.historyBaseCols = evicted.cols;
    ch.historyBaseRows = evicted.rows;
    removeCount++;
  }
  if (removeCount > 0) ch.historyResizes.splice(0, removeCount);
}

function releaseOutput(
  ch: Channel,
  channelId: number,
  chunk: Uint8Array,
  broadcast: (frame: Buffer) => void,
): void {
  ch.headSeq += chunk.byteLength;
  appendToRing(ch.outRing, chunk);
  trimEvictedResizeHistory(ch);
  broadcast(encodeMuxFrame(MuxFrameType.PtyOut, channelId, chunk));
}

function bufferOrReleaseOutput(
  ch: Channel,
  channelId: number,
  chunk: Uint8Array,
  broadcast: (frame: Buffer) => void,
): void {
  const boundary = ch.outputBoundaryBuffer;
  if (!boundary) {
    releaseOutput(ch, channelId, chunk, broadcast);
    return;
  }
  const retained = Buffer.from(chunk);
  boundary.chunks.push(retained);
  boundary.bytes += retained.byteLength;
}

function releaseBoundaryOutput(
  ch: Channel,
  channelId: number,
  broadcast: (frame: Buffer) => void,
): void {
  const boundary = ch.outputBoundaryBuffer;
  ch.outputBoundaryBuffer = null;
  if (!boundary) return;
  for (const chunk of boundary.chunks) releaseOutput(ch, channelId, chunk, broadcast);
}

function appendResizeHistory(ch: Channel, seq: number, cols: number, rows: number): void {
  // A resize-only flood must not grow an unbounded marker list. If its marker
  // budget is exhausted, discard the raw window too; retaining fewer records
  // is truthful, while retaining bytes under an unknowable geometry is not.
  if (ch.historyResizes.length >= KEEPER_MAX_HISTORY_RESIZE_RECORDS) {
    ch.outRing.buf = EMPTY_U8;
    ch.outRing.write = 0;
    ch.outRing.filled = 0;
    ch.historyResizes.length = 0;
    ch.historyBaseCols = ch.currentCols;
    ch.historyBaseRows = ch.currentRows;
  }
  ch.historyResizes.push({ headSeq: ch.headSeq, seq, cols, rows });
  ch.currentCols = cols;
  ch.currentRows = rows;
}

function orderedHistory(ch: Channel): KeeperHistoryRecords {
  const retained = readRing(ch.outRing);
  const retainedTail = ch.headSeq - retained.byteLength;
  const records: KeeperHistoryRecord[] = [];
  let rawSeq = retainedTail;
  for (const resize of ch.historyResizes) {
    if (resize.headSeq < retainedTail || resize.headSeq > ch.headSeq) continue;
    const outputBytes = resize.headSeq - rawSeq;
    if (outputBytes > 0) {
      const offset = rawSeq - retainedTail;
      records.push({ kind: "output", bytes: retained.subarray(offset, offset + outputBytes) });
      rawSeq = resize.headSeq;
    }
    records.push({
      kind: "resize",
      seq: resize.seq,
      cols: resize.cols,
      rows: resize.rows,
    });
  }
  if (rawSeq < ch.headSeq) {
    records.push({ kind: "output", bytes: retained.subarray(rawSeq - retainedTail) });
  }
  return {
    headSeq: ch.headSeq,
    baseCols: ch.historyBaseCols,
    baseRows: ch.historyBaseRows,
    records,
  };
}

function sendPtyInResult(
  socket: ClientState["socket"],
  channelId: number,
  result: PtyInWireResult,
): void {
  const frameType = result.kind === "ack"
    ? MuxFrameType.PtyInAck
    : result.kind === "reject"
      ? MuxFrameType.PtyInReject
      : MuxFrameType.PtyInAmbiguous;
  try {
    socket.write(encodeMuxFrame(frameType, channelId, encodePtyInResult(result)));
  } catch {
    // The client resolves an outstanding command conservatively on close.
  }
}

async function drainInputQueue(channelId: number, ch: Channel): Promise<void> {
  if (ch.inputWriting) return;
  ch.inputWriting = true;
  try {
    while (ch.inputQueue.length > 0) {
      const batch = ch.inputQueue[0]!;
      if (!batch.started && batch.socket?.destroyed) {
        ch.inputQueue.shift();
        ch.inputQueueBytes -= batch.bytes.byteLength;
        continue;
      }
      batch.started = true;
      const deadline = Date.now() + KEEPER_INPUT_DEADLINE_MS;
      let writtenBytes = 0;
      let result: PtyInWireResult | null = null;
      while (writtenBytes < batch.bytes.byteLength) {
        if (ch.exited || ch.terminal.closed) {
          result = writtenBytes === 0
            ? { kind: "reject", inputSeq: batch.inputSeq ?? 1, writtenBytes: 0, reason: "channel_exited" }
            : { kind: "ambiguous", inputSeq: batch.inputSeq ?? 1, writtenBytes, reason: "channel_exited" };
          break;
        }
        if (Date.now() >= deadline) {
          result = writtenBytes === 0
            ? { kind: "reject", inputSeq: batch.inputSeq ?? 1, writtenBytes: 0, reason: "deadline" }
            : { kind: "ambiguous", inputSeq: batch.inputSeq ?? 1, writtenBytes, reason: "deadline" };
          break;
        }
        let count: number;
        try {
          count = ch.terminal.write(batch.bytes.subarray(writtenBytes));
        } catch (error) {
          _log("warn", "multiplexed-keeper", "ptyin_write_failed", {
            channelId,
            error: String(error),
          });
          result = writtenBytes === 0
            ? { kind: "reject", inputSeq: batch.inputSeq ?? 1, writtenBytes: 0, reason: "write_error" }
            : { kind: "ambiguous", inputSeq: batch.inputSeq ?? 1, writtenBytes, reason: "write_error" };
          break;
        }
        const remaining = batch.bytes.byteLength - writtenBytes;
        if (!Number.isInteger(count) || count < 0 || count > remaining) {
          result = writtenBytes === 0
            ? { kind: "reject", inputSeq: batch.inputSeq ?? 1, writtenBytes: 0, reason: "invalid_write_count" }
            : { kind: "ambiguous", inputSeq: batch.inputSeq ?? 1, writtenBytes, reason: "invalid_write_count" };
          break;
        }
        if (count > 0) {
          writtenBytes += count;
          continue;
        }
        await new Promise<void>(resolve => setTimeout(resolve, 0));
      }
      if (!result) {
        result = {
          kind: "ack",
          inputSeq: batch.inputSeq ?? 1,
          writtenBytes: batch.bytes.byteLength,
        };
      }
      if (batch.inputSeq !== null && batch.socket) {
        sendPtyInResult(batch.socket, channelId, result);
      }
      ch.inputQueue.shift();
      ch.inputQueueBytes -= batch.bytes.byteLength;
    }
  } finally {
    ch.inputWriting = false;
  }
}

function enqueueInput(
  channelId: number,
  ch: Channel,
  bytes: Buffer,
  inputSeq: number | null,
  socket: ClientState["socket"] | null,
): boolean {
  if (ch.inputQueue.length >= KEEPER_INPUT_QUEUE_MAX_COMMANDS
      || ch.inputQueueBytes + bytes.byteLength > KEEPER_INPUT_QUEUE_MAX_BYTES) {
    return false;
  }
  ch.inputQueue.push({ inputSeq, bytes, socket, started: false });
  ch.inputQueueBytes += bytes.byteLength;
  void drainInputQueue(channelId, ch).catch(error => {
    ch.inputWriting = false;
    _log("error", "multiplexed-keeper", "input_queue_failed", {
      channelId,
      error: String(error),
    });
  });
  return true;
}

function sendResizeResult(
  socket: ClientState["socket"],
  channelId: number,
  result: ResizeWireResult,
): void {
  const frameType = result.kind === "ack" ? MuxFrameType.ResizeAck : MuxFrameType.ResizeReject;
  try {
    socket.write(encodeMuxFrame(frameType, channelId, encodeResizeResult(result)));
  } catch {
    // Cached status remains queryable after the worker reconnects.
  }
}

function cacheResizeResult(ch: Channel, result: ResizeWireResult): void {
  if (!ch.resizeStatuses.has(result.seq)
      && ch.resizeStatuses.size >= RESIZE_STATUS_CACHE_MAX) {
    const oldest = ch.resizeStatuses.keys().next().value as number | undefined;
    if (oldest !== undefined) ch.resizeStatuses.delete(oldest);
  }
  ch.resizeStatuses.set(result.seq, result);
  ch.highestResizeSeq = Math.max(ch.highestResizeSeq, result.seq);
}

export interface FrameHandlerCtx {
  channels: Map<number, Channel>;
  broadcast: (frame: Buffer) => void;
}

export function handleFrame(ctx: FrameHandlerCtx, client: ClientState, f: { type: MuxFrameType; channelId: number; payload: Buffer }): void {
  const { channels, broadcast } = ctx;
  switch (f.type) {
    case MuxFrameType.Spawn: {
      const req = decodeSpawnRequest(f.payload);
      if (!req || req.channel_id !== f.channelId || f.channelId === 0) {
        _log("error", "multiplexed-keeper", "spawn_decode_failed", { payload_len: f.payload.length });
        return;
      }
      if (channels.has(req.channel_id) || SPAWNING_CHANNELS.has(req.channel_id)) {
        _log("warn", "multiplexed-keeper", "spawn_channel_in_use", { channelId: req.channel_id });
        client.socket.write(encodeMuxFrame(MuxFrameType.SpawnErr, req.channel_id, JSON.stringify({ error: "channel_id in use" })));
        return;
      }
      const spec = req.shell_spec;
      const runtimePlatform = supportedHostPlatform();
      if (spec.platform !== runtimePlatform) {
        client.socket.write(encodeMuxFrame(
          MuxFrameType.SpawnErr,
          req.channel_id,
          JSON.stringify({ error: `shell platform ${spec.platform} does not match keeper ${runtimePlatform}` }),
        ));
        return;
      }
      const spawnedAtMs = Date.now();
      SPAWNING_CHANNELS.add(req.channel_id);
      void (async () => {
        let terminal: Bun.Terminal | undefined;
        let jobHost: WindowsJobHostHandle | undefined;
        try {
          const earlyOutput: Buffer[] = [];
          let acceptingEarlyOutput = true;
          const emitOutput = (data: Uint8Array): void => {
            const channel = channels.get(req.channel_id);
            if (!channel) {
              // Bun may reuse the callback's backing store before an async
              // Windows job-host spawn completes.
              if (acceptingEarlyOutput) earlyOutput.push(Buffer.from(data));
              return;
            }
            // Normal output is consumed synchronously into the ring/final
            // frame. Boundary buffering makes its own defensive copy.
            bufferOrReleaseOutput(channel, req.channel_id, data, broadcast);
          };
          terminal = new Bun.Terminal({
            cols: req.cols,
            rows: req.rows,
            data: (_terminal, data) => emitOutput(data),
          });

          let proc: Bun.Subprocess;
          let childPid: number;
          switch (runtimePlatform) {
            case "darwin":
            case "linux":
              proc = Bun.spawn([spec.executable, ...spec.argv], {
                cwd: spec.cwd,
                terminal,
                env: { ...spec.env, TERM: "xterm-256color" },
              });
              childPid = proc.pid;
              break;
            case "win32":
              jobHost = await spawnWindowsJobHost({
                terminal,
                executable: spec.executable,
                argv: spec.argv,
                cwd: nativePathToFsPath("win32", spec.cwd),
                env: spec.env,
              });
              proc = jobHost.process;
              childPid = jobHost.assignedPid;
              break;
            default:
              return assertNeverPlatform(runtimePlatform);
          }

          const ch: Channel = {
            proc,
            terminal,
            jobHost,
            childPid,
            exited: false,
            outRing: createSbRing(undefined, KEEPER_RING_CAP_BYTES),
            headSeq: 0,
            historyBaseCols: req.cols,
            historyBaseRows: req.rows,
            historyResizes: [],
            currentCols: req.cols,
            currentRows: req.rows,
            outputBoundaryBuffer: null,
            resizeStatuses: new Map(),
            highestResizeSeq: 0,
            inputQueue: [],
            inputQueueBytes: 0,
            inputWriting: false,
          };
          channels.set(req.channel_id, ch);
          for (const chunk of earlyOutput) emitOutput(chunk);
          acceptingEarlyOutput = false;
          earlyOutput.length = 0;
          _log("info", "multiplexed-keeper", "child_spawned", {
            channelId: req.channel_id,
            pid: childPid,
            host_pid: jobHost?.process.pid,
            argv0: spec.executable,
            cwd: spec.cwd,
          });

          void proc.exited.then(async (processExitCode) => {
            const exitCode = jobHost ? (await jobHost.closed).exitCode : processExitCode;
            ch.exited = true;
            const lifetimeMs = Date.now() - spawnedAtMs;
            const deadBirth = lifetimeMs < 2000 && ch.headSeq === 0;
            _log(deadBirth ? "warn" : "info", "multiplexed-keeper", deadBirth ? "child_dead_birth" : "child_exited", {
              channelId: req.channel_id,
              pid: childPid,
              host_pid: jobHost?.process.pid,
              argv0: spec.executable,
              cwd: spec.cwd,
              exit_code: exitCode,
              signal: proc.signalCode ?? null,
              lifetime_ms: lifetimeMs,
              head_seq: ch.headSeq,
              ...(deadBirth ? { open_fds: _keeperOpenFdCount(), live_channels: channels.size } : {}),
            });
            broadcast(encodeMuxFrame(MuxFrameType.Exit, req.channel_id, JSON.stringify({ exit_code: exitCode })));
            channels.delete(req.channel_id);
            // A Windows close status proves ACTIVE_PROCESS_ZERO. Only after it
            // (or POSIX process exit) is it safe to release the PTY master.
            try { terminal?.close(); } catch { /* already closed */ }
          }).catch((error) => {
            _log("error", "multiplexed-keeper", jobHost ? "job_host_close_unproven" : "exit_handler_failed", {
              channelId: req.channel_id,
              pid: childPid,
              host_pid: jobHost?.process.pid,
              error: String(error),
            });
            ch.exited = true;
            channels.delete(req.channel_id);
            broadcast(encodeMuxFrame(MuxFrameType.Exit, req.channel_id, JSON.stringify({ exit_code: null })));
            try { terminal?.close(); } catch { /* process is already gone */ }
          });

          client.socket.write(encodeMuxFrame(MuxFrameType.SpawnAck, req.channel_id, JSON.stringify({ pid: childPid })));
        } catch (error) {
          if (jobHost) {
            try { await jobHost.close(); }
            catch (closeError) {
              _log("error", "multiplexed-keeper", "job_host_spawn_cleanup_unproven", {
                channelId: req.channel_id,
                error: String(closeError),
              });
            }
          }
          try { terminal?.close(); } catch { /* no live host owns it */ }
          _log("error", "multiplexed-keeper", "spawn_failed", {
            channelId: req.channel_id,
            argv0: spec.executable,
            cwd: spec.cwd,
            error: String(error),
            open_fds: _keeperOpenFdCount(),
            live_channels: channels.size,
          });
          client.socket.write(encodeMuxFrame(MuxFrameType.SpawnErr, req.channel_id, JSON.stringify({ error: String(error) })));
        } finally {
          SPAWNING_CHANNELS.delete(req.channel_id);
        }
      })();
      return;
    }
    case MuxFrameType.PtyIn: {
      const ch = channels.get(f.channelId);
      if (!ch || ch.exited || f.payload.byteLength === 0
          || f.payload.byteLength > KEEPER_MAX_INPUT_BYTES) {
        return;
      }
      // Legacy callers do not receive an ACK, but share the same FIFO so they
      // cannot interleave with an acknowledged batch.
      if (!enqueueInput(f.channelId, ch, Buffer.from(f.payload), null, null)) {
        _log("warn", "multiplexed-keeper", "legacy_input_queue_full", {
          channelId: f.channelId,
          bytes: f.payload.byteLength,
        });
      }
      return;
    }
    case MuxFrameType.PtyInRequest: {
      const request = decodePtyInRequest(f.payload);
      if (!request) {
        _log("warn", "multiplexed-keeper", "ptyin_request_decode_failed", {
          channelId: f.channelId,
          payload_len: f.payload.byteLength,
        });
        return;
      }
      const ch = channels.get(f.channelId);
      if (!ch) {
        sendPtyInResult(client.socket, f.channelId, {
          kind: "reject",
          inputSeq: request.inputSeq,
          writtenBytes: 0,
          reason: "channel_missing",
        });
        return;
      }
      if (ch.exited || ch.terminal.closed) {
        sendPtyInResult(client.socket, f.channelId, {
          kind: "reject",
          inputSeq: request.inputSeq,
          writtenBytes: 0,
          reason: "channel_exited",
        });
        return;
      }
      const bytes = Buffer.from(request.bytes);
      if (!enqueueInput(f.channelId, ch, bytes, request.inputSeq, client.socket)) {
        sendPtyInResult(client.socket, f.channelId, {
          kind: "reject",
          inputSeq: request.inputSeq,
          writtenBytes: 0,
          reason: "queue_full",
        });
      }
      return;
    }
    case MuxFrameType.Resize: {
      const ch = channels.get(f.channelId);
      if (!ch || ch.exited || ch.terminal.closed) return;
      try {
        const value: unknown = JSON.parse(f.payload.toString("utf8"));
        if (!value || typeof value !== "object" || Array.isArray(value)) return;
        const request = value as { cols?: unknown; rows?: unknown };
        if (typeof request.cols !== "number" || !Number.isInteger(request.cols)
            || request.cols <= 0 || request.cols > KEEPER_MAX_TERMINAL_DIMENSION
            || typeof request.rows !== "number" || !Number.isInteger(request.rows)
            || request.rows <= 0 || request.rows > KEEPER_MAX_TERMINAL_DIMENSION) {
          return;
        }
        ch.outputBoundaryBuffer = { chunks: [], bytes: 0 };
        try {
          ch.terminal.resize(request.cols, request.rows);
          // Sequence zero is reserved for the deployed legacy frame.
          appendResizeHistory(ch, 0, request.cols, request.rows);
        } finally {
          releaseBoundaryOutput(ch, f.channelId, broadcast);
        }
      } catch (error) {
        _log("warn", "multiplexed-keeper", "legacy_resize_failed", {
          channelId: f.channelId,
          error: String(error),
        });
      }
      return;
    }
    case MuxFrameType.ResizeRequest: {
      const request = decodeResizeRequest(f.payload);
      if (!request) {
        _log("warn", "multiplexed-keeper", "resize_request_decode_failed", {
          channelId: f.channelId,
          payload_len: f.payload.byteLength,
        });
        return;
      }
      const ch = channels.get(f.channelId);
      if (!ch) {
        sendResizeResult(client.socket, f.channelId, {
          kind: "reject",
          seq: request.seq,
          reason: "channel_missing",
        });
        return;
      }
      const cached = ch.resizeStatuses.get(request.seq);
      if (cached) {
        sendResizeResult(client.socket, f.channelId, cached);
        return;
      }
      if (request.seq <= ch.highestResizeSeq) {
        sendResizeResult(client.socket, f.channelId, {
          kind: "reject",
          seq: request.seq,
          reason: "unknown_sequence",
        });
        return;
      }

      let result: ResizeWireResult;
      let ownsBoundary = false;
      if (ch.exited || ch.terminal.closed) {
        result = { kind: "reject", seq: request.seq, reason: "channel_exited" };
      } else if (ch.outputBoundaryBuffer) {
        result = { kind: "reject", seq: request.seq, reason: "resize_error" };
      } else {
        ch.outputBoundaryBuffer = { chunks: [], bytes: 0 };
        ownsBoundary = true;
        try {
          ch.terminal.resize(request.cols, request.rows);
          appendResizeHistory(ch, request.seq, request.cols, request.rows);
          result = {
            kind: "ack",
            seq: request.seq,
            cols: request.cols,
            rows: request.rows,
          };
        } catch (error) {
          _log("warn", "multiplexed-keeper", "resize_failed", {
            channelId: f.channelId,
            seq: request.seq,
            error: String(error),
          });
          result = { kind: "reject", seq: request.seq, reason: "resize_error" };
        }
      }
      cacheResizeResult(ch, result);
      sendResizeResult(client.socket, f.channelId, result);
      if (ownsBoundary) releaseBoundaryOutput(ch, f.channelId, broadcast);
      return;
    }
    case MuxFrameType.ResizeStatus: {
      const query = decodeResizeStatusQuery(f.payload);
      if (!query) {
        _log("warn", "multiplexed-keeper", "resize_status_decode_failed", {
          channelId: f.channelId,
          payload_len: f.payload.byteLength,
        });
        return;
      }
      const ch = channels.get(f.channelId);
      const cached = ch?.resizeStatuses.get(query.seq);
      sendResizeResult(client.socket, f.channelId, cached ?? {
        kind: "reject",
        seq: query.seq,
        reason: ch ? "unknown_sequence" : "channel_missing",
      });
      return;
    }
    case MuxFrameType.KillChild: {
      const ch = channels.get(f.channelId);
      if (!ch || ch.exited) return;
      void reapChannelTree(ch).catch(error => {
        _log("error", "multiplexed-keeper", "kill_child_failed", {
          channelId: f.channelId,
          error: String(error),
        });
      });
      return;
    }
    case MuxFrameType.Ping: {
      client.socket.write(encodeMuxFrame(MuxFrameType.Pong, 0, Buffer.alloc(0)));
      return;
    }
    case MuxFrameType.ListChannels: {
      const list: Array<{ channel_id: number; pid: number }> = [];
      for (const [channelId, ch] of channels.entries()) {
        if (!ch.exited) list.push({ channel_id: channelId, pid: ch.childPid });
      }
      client.socket.write(encodeMuxFrame(
        MuxFrameType.ListChannelsResp, 0,
        JSON.stringify({ channels: list }),
      ));
      return;
    }
    case MuxFrameType.Hello: {
      client.socket.write(encodeMuxFrame(
        MuxFrameType.HelloResp, 0,
        JSON.stringify({ version: KEEPER_PROTOCOL_VERSION, build: KEEPER_BUILD_STAMP }),
      ));
      return;
    }
    case MuxFrameType.GetHistory: {
      // RC2: reply with [8-byte BE head_seq][ring bytes] for the channel.
      // Unknown/exited channel → head_seq 0 + empty ring (worker treats it
      // as a fresh session, same as the pre-RC2 zeroing behavior).
      const ch = channels.get(f.channelId);
      const headSeq = ch ? ch.headSeq : 0;
      const ring = ch ? readRing(ch.outRing) : EMPTY_U8;
      const head = Buffer.allocUnsafe(8);
      head.writeBigUInt64BE(BigInt(headSeq), 0);
      client.socket.write(encodeMuxFrame(
        MuxFrameType.GetHistoryResp, f.channelId, Buffer.concat([head, ring]),
      ));
      return;
    }
    case MuxFrameType.GetHistoryRecords: {
      if (!isEmptyKeeperPayload(f.payload)) {
        _log("warn", "multiplexed-keeper", "history_records_payload_not_empty", {
          channelId: f.channelId,
          payload_len: f.payload.byteLength,
        });
        return;
      }
      const ch = channels.get(f.channelId);
      const history = ch ? orderedHistory(ch) : {
        headSeq: 0,
        baseCols: 80,
        baseRows: 24,
        records: [],
      };
      client.socket.write(encodeMuxFrame(
        MuxFrameType.GetHistoryRecordsResp,
        f.channelId,
        encodeKeeperHistoryRecords(history),
      ));
      return;
    }
    default: return;
  }
}
