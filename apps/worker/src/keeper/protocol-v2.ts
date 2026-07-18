// T2.1 — multiplexed keeper protocol. A single keeper process hosts N
// PTYs; each frame carries a channel_id so the worker side can demux.
//
// Frame format:
//   [4-byte BE uint32 total_length] (covers everything after these 4 bytes)
//   [1-byte type tag]
//   [2-byte BE uint16 channel_id]   (0 = control / global)
//   [payload bytes]
//
// Stable, never renumber the type tags.

/** Frame type tags. */
export const enum MuxFrameType {
  Spawn      = 0x10, // client → keeper: JSON {channel_id, cwd, cols, rows, env?} — open a new PTY
  SpawnAck   = 0x11, // keeper → client: JSON {channel_id, pid} — PTY allocated
  SpawnErr   = 0x12, // keeper → client: JSON {channel_id, error}

  PtyIn      = 0x20, // client → keeper: raw input bytes (per channel)
  PtyOut     = 0x21, // keeper → client: raw output bytes (per channel)

  Resize     = 0x30, // client → keeper: JSON {cols, rows}
  KillChild  = 0x31, // client → keeper: no payload — SIGTERM the PTY child
  Exit       = 0x32, // keeper → client: JSON {exit_code: number | null}

  Ping       = 0xF0, // both, global (channel=0): liveness probe
  Pong       = 0xF1,

  // Cross-process resume — added so a fresh worker can discover the
  // channels surviving in a long-lived multiplexed-keeper process.
  ListChannels     = 0xE0, // client → keeper: no payload (channel=0)
  ListChannelsResp = 0xE1, // keeper → client: JSON {channels:[{channel_id,pid}]}

  // Version handshake — fresh worker probes a survivor keeper for
  // protocol compatibility before trusting it for resume. Keepers from
  // a prior commit either pre-date this frame (silent drop → handshake
  // timeout → treat as incompatible) or reply with a stale version
  // number. See KEEPER_PROTOCOL_VERSION below + apps/worker/src/main.ts
  // killStaleKeeper gate.
  Hello            = 0xE2, // client → keeper: JSON {version:number} (channel=0)
  HelloResp        = 0xE3, // keeper → client: JSON {version:number, build?:string}
  // `build` = KEEPER_BUILD_STAMP (keeper-stamp.ts), ADDITIVE + NON-gating: it
  // surfaces stale keeper CODE (probeKeeperCompatible reports it) but does NOT
  // enter the kill gate, so a pre-stamp keeper (no `build`) is not killed —
  // only flagged. Adding it needs NO version bump (absence is detectable).

  // RC2 cross-process history resume — a fresh worker re-reads a surviving
  // channel's seqno-stamped output ring so resume() restores head_seq +
  // scrollback instead of zeroing (history survives a worker restart; the
  // keeper outlives the worker). ADDITIVE: a keeper from a prior commit
  // silently drops GetHistory → client times out → resume() falls back to
  // the old empty-ring behavior. Absence is detectable, so NO version bump
  // (which would kill the running keeper via killStaleKeeper).
  GetHistory       = 0xE4, // client → keeper: no payload (per channel)
  GetHistoryResp   = 0xE5, // keeper → client: [8-byte BE head_seq][ring bytes]
}

/** Wire-protocol version. BUMP whenever:
 *  - any existing frame's payload JSON shape changes
 *  - a frame's tag number is reassigned (never do this; add a new tag)
 *  - the encoding of any frame changes (e.g. switching JSON → binary)
 *
 *  A bump invalidates every running keeper subprocess: the next worker
 *  boot probes via Hello, sees a mismatch, kills + unlinks the stale
 *  keeper. Bumping is OPT-IN for backwards-compatible additions —
 *  appending a new frame tag whose absence is detectable (e.g. via
 *  Hello payload) does NOT require a bump.
 *
 *  Bump log:
 *    1 — initial Hello/HelloResp handshake (2026-06-18) */
export const KEEPER_PROTOCOL_VERSION = 1;

export interface MuxFrame {
  type: MuxFrameType;
  channelId: number;
  payload: Buffer;
}

/** Encode a single multiplexed frame. */
export function encodeMuxFrame(
  type: MuxFrameType, channelId: number, payload: Uint8Array | string,
): Buffer {
  const payloadBuf = typeof payload === "string"
    ? Buffer.from(payload, "utf8")
    : Buffer.from(payload);
  const total = 1 + 2 + payloadBuf.length;
  const out = Buffer.allocUnsafe(4 + total);
  out.writeUInt32BE(total, 0);
  out[4] = type;
  out.writeUInt16BE(channelId, 5);
  payloadBuf.copy(out, 7);
  return out;
}

/** Decode multiplexed frames from a streaming buffer. */
export function decodeMuxFrames(buf: Buffer): {
  frames: MuxFrame[];
  remaining: Buffer;
} {
  const frames: MuxFrame[] = [];
  let offset = 0;
  while (offset + 4 <= buf.length) {
    const total = buf.readUInt32BE(offset);
    if (total === 0) { offset += 4; continue; }
    if (offset + 4 + total > buf.length) break;
    const type = buf[offset + 4] as MuxFrameType;
    const channelId = buf.readUInt16BE(offset + 5);
    // subarray returns a Buffer view sharing the underlying ArrayBuffer.
    // Consumers in multiplexed-client.ts (onOutput/spawnAck/etc.) read
    // the payload synchronously before the next _onData replaces
    // this.buf, so the view stays valid — the prior Buffer.from copy
    // was defensive against an async retention pattern that never
    // existed.
    frames.push({ type, channelId, payload: buf.subarray(offset + 7, offset + 4 + total) });
    offset += 4 + total;
  }
  return { frames, remaining: offset > 0 ? buf.subarray(offset) : buf };
}

export interface SpawnRequest {
  channel_id: number;
  cwd: string;
  cols: number;
  rows: number;
  argv?: string[];
  env?: Record<string, string>;
}

export function encodeSpawnRequest(req: SpawnRequest): string {
  return JSON.stringify(req);
}

export function decodeSpawnRequest(payload: Buffer): SpawnRequest | null {
  try {
    const v = JSON.parse(payload.toString("utf8"));
    if (typeof v.channel_id === "number" && typeof v.cwd === "string"
        && typeof v.cols === "number" && typeof v.rows === "number") {
      return v as SpawnRequest;
    }
  } catch { /* fall through */ }
  return null;
}
