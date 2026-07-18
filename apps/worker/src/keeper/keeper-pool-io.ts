// Multiplexed keeper pool — per-channel IO (input/resize/kill), the shared
// null-socket-aware frame writer, and pool teardown. Free functions extracted
// from MultiplexedKeeperPool (multiplexed-client.ts); each takes the pool
// instance as its first argument. Behavior is unchanged.

import { unlinkSync } from "node:fs";
import { log, signal } from "@roost/shared";
import { MuxFrameType, encodeMuxFrame } from "./protocol-v2.ts";
import { muxSocketPath } from "./keeper-pool-config.ts";
import type { MultiplexedKeeperPool } from "./multiplexed-client.ts";

const DROP_LOG_INTERVAL_MS = 1000;

/** Write a frame to the keeper socket with consistent null-socket
 *  handling. `logTarget` null = silent drop (kill path); otherwise
 *  log.warn so the gap is visible (rate-limited to one line per
 *  second per target). The close handler (line ~122) already fired
 *  onExit(null) + channels.clear() for any channel the worker thinks
 *  is alive, so replaying frames after reconnect is unsafe — the
 *  new keeper PID doesn't know the prior channelId. */
function writeFrame(pool: MultiplexedKeeperPool, frame: Buffer, logTarget: string | null, dropFields: Record<string, unknown>): void {
  if (!pool.socket) {
    if (logTarget !== null) {
      const now = Date.now();
      const state = pool._dropLogState.get(logTarget) ?? { lastTs: 0, suppressed: 0 };
      if (now - state.lastTs >= DROP_LOG_INTERVAL_MS) {
        log.warn(logTarget, "dropped: socket closed", { ...dropFields, suppressedSince: state.suppressed });
        pool._dropLogState.set(logTarget, { lastTs: now, suppressed: 0 });
      } else {
        pool._dropLogState.set(logTarget, { lastTs: state.lastTs, suppressed: state.suppressed + 1 });
      }
      // User input (PtyIn) dropped to a dead keeper socket = keystrokes lost.
      // `input.drop_burst` is Tier-1 (signal cooldown coalesces the flood).
      if (logTarget === "mux-pool.input") {
        signal("input.drop_burst", {
          sid: dropFields.sid,
          reason: "keeper_socket_closed",
          cooldownKey: dropFields.sid,
        });
      }
    }
    return;
  }
  pool.socket.write(frame);
}

export function channelInput(pool: MultiplexedKeeperPool, channelId: number, bytes: Uint8Array): void {
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.PtyIn, channelId, bytes),
    "mux-pool.input",
    { channelId, len: bytes.length },
  );
}

export function channelResize(pool: MultiplexedKeeperPool, channelId: number, cols: number, rows: number): void {
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.Resize, channelId, JSON.stringify({ cols, rows })),
    "mux-pool.resize",
    { channelId, cols, rows },
  );
}

export function channelKill(pool: MultiplexedKeeperPool, channelId: number): void {
  // Silent on null socket (expected — close handler already cascaded
  // through SessionManager.closedByKeeper).
  writeFrame(
    pool,
    encodeMuxFrame(MuxFrameType.KillChild, channelId, new Uint8Array(0)),
    null,
    {},
  );
}

export function disposePool(pool: MultiplexedKeeperPool): void {
  if (pool.socket) {
    try { pool.socket.destroy(); } catch { /* ignore */ }
    pool.socket = null;
  }
  try { unlinkSync(muxSocketPath()); } catch { /* ignore */ }
}
