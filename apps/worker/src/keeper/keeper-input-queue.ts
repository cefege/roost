// Acknowledged PTY-input lane for the multiplexed keeper: per-channel FIFO,
// byte/command budgets, write deadline and the conservative result frames the
// worker resolves against. Legacy (unacknowledged) input shares the same FIFO
// so the two lanes can never interleave mid-batch. Called from the frame
// handler; owns nothing — queue state lives on each Channel.

import {
  MuxFrameType,
  encodeMuxFrame,
  encodePtyInResult,
} from "./protocol.ts";
import type { PtyInWireResult } from "./protocol.ts";
import { _log } from "./keeper-log.ts";
import type { Channel, ClientState } from "./keeper-types.ts";

const KEEPER_INPUT_QUEUE_MAX_COMMANDS = 200;
const KEEPER_INPUT_QUEUE_MAX_BYTES = 256 * 1024;

export function sendPtyInResult(
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
      let result: PtyInWireResult;
      if (ch.exited || ch.terminal.closed) {
        result = {
          kind: "reject",
          inputSeq: batch.inputSeq ?? 1,
          writtenBytes: 0,
          reason: "channel_exited",
        };
      } else {
        try {
          // Bun buffers the whole batch before returning; the numeric return is
          // not a short-write contract and must never drive a resend.
          ch.terminal.write(batch.bytes);
          result = {
            kind: "ack",
            inputSeq: batch.inputSeq ?? 1,
            writtenBytes: batch.bytes.byteLength,
          };
        } catch (error) {
          _log("warn", "multiplexed-keeper", "ptyin_write_failed", {
            channelId,
            error: String(error),
          });
          result = {
            kind: "reject",
            inputSeq: batch.inputSeq ?? 1,
            writtenBytes: 0,
            reason: "write_error",
          };
        }
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

export function enqueueInput(
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
