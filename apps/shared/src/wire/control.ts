// ControlFrame = raw browser↔worker WSS protocol. Tagged union.
// PTY bytes use a separate binary frame to avoid base64 overhead;
// this Zod schema validates only the JSON control plane.

import { z } from "zod";
import { ChannelId, SessionId, TraceId } from "./brand.ts";

/** Why a get-scrollback-cells page came back short of the requested range —
 *  which history floor the caller hit. Mirrors roost.v1.ScrollbackHistoryFloor
 *  and is the wire form the worker stamps on its rpc-ok data.
 *
 *  "none"          the full requested range was served; no floor was hit.
 *  "evicted"       gone forever: the core's own line ring rolled past those
 *                  rows as newer output arrived.
 *  "resize_replay" lost to the bounded keeper history available during worker
 *                  adoption, where no in-memory core survived. Ordinary live
 *                  resize is in place and never creates this floor. */
export type ScrollbackHistoryFloor = "none" | "evicted" | "resize_replay";

const Base = z.object({ trace_id: TraceId.optional() });
// ─── client → worker ───────────────────────────────────────────────────

export const ClientControlFrame = z.discriminatedUnion("kind", [
  // attach a viewer to a session — N attachers per session allowed
  Base.extend({ kind: z.literal("attach"), session_id: SessionId, from_offset: z.number().int().nonnegative().optional() }),
  Base.extend({ kind: z.literal("detach"), session_id: SessionId }),
  Base.extend({
    kind: z.literal("spawn-shell"),
    folder: z.string(),
    // SPA passes its current wterm cols/rows so the keeper PTY starts
    // at the right size from the first byte. Without this, TUIs read
    // the keeper default (220×50) and paint to that width, then only
    // SIGWINCH-redraw after our resize lands → the pre-resize paint
    // leaves wrap/tear in the visible buffer.
    cols: z.number().int().positive().optional(),
    rows: z.number().int().positive().optional(),
    // caller-minted id (optimistic spawn); worker reuses it verbatim
    session_id: SessionId.optional(),
  }),
  Base.extend({ kind: z.literal("kill"), session_id: SessionId }),
  Base.extend({ kind: z.literal("read-file"), request_id: z.string(), path: z.string(), max_lines: z.number().int().positive().optional() }),
  // chunked byte-range read backing the SPA's progress-tracking download.
  // rpc-ok data: { content_b64, size, eof }.
  Base.extend({ kind: z.literal("read-file-chunk"), request_id: z.string(), path: z.string(), offset: z.number().int().nonnegative(), len: z.number().int().positive() }),
  // content-dedup probe: does this session's attachment dir already hold a file
  // with this SHA-256? rpc-ok data: { hit, abs_path }.
  Base.extend({ kind: z.literal("attachment-probe"), request_id: z.string(), session_id: SessionId, sha256: z.string(), short_path: z.boolean() }),
  Base.extend({ kind: z.literal("list-dir"), request_id: z.string(), path: z.string() }),
  Base.extend({ kind: z.literal("mkdir"), request_id: z.string(), path: z.string() }),
  Base.extend({ kind: z.literal("list-skills"), request_id: z.string() }),
  Base.extend({ kind: z.literal("git-diff"), request_id: z.string(), session_id: SessionId }),
  Base.extend({ kind: z.literal("set-title"), session_id: SessionId, title: z.string() }),
  // cursor position update for presence — browser sends whenever wterm cursor moves
  Base.extend({ kind: z.literal("cursor-pos"), session_id: SessionId, col: z.number().int().nonnegative(), row: z.number().int().nonnegative() }),
  // request the worker's $HOME directory — response is rpc-ok { home: string }
  Base.extend({ kind: z.literal("get-home"), request_id: z.string() }),
  // Demand-driven history page from a stable grid epoch. Browsers name the
  // epoch of their authoritative frame; an empty headless/API epoch binds to
  // the worker's current epoch, which is returned in rpc-ok:
  // { rows: CellRow[], cols, total, start_row, end_row, grid_epoch, history_floor }.
  // A page clamped at the retained floor comes back SHORT, and `history_floor`
  // (ScrollbackHistoryFloor above) says which floor that was, so the caller can
  // stop paging and name the cause instead of retrying forever.
  Base.extend({
    kind: z.literal("get-scrollback-cells"),
    request_id: z.string(),
    session_id: SessionId,
    grid_epoch: z.string(),
    end_row: z.number().int().nonnegative(),
    max_rows: z.number().int().positive(),
  }),
  // Find-in-scrollback (G): the SPA holds at most MAX_HELD_SCROLLBACK_ROWS of
  // the worker's retained history, so the search runs against the worker's
  // authoritative grid instead. rpc-ok data:
  // { matches: [{ row, col, len, preview }], truncated, total, cols, grid_epoch }
  // — `row` is the MONOTONIC absolute index (same space as PbCellRow.index /
  // sbBase) IN `grid_epoch`, newest row first. Epoch-fenced like
  // get-scrollback-cells: a named epoch that is no longer current rejects with
  // "grid epoch changed" rather than answering from a re-numbered grid; an empty
  // headless/API epoch binds to the worker's current one. Invalid `query` under
  // regex → rpc-error "invalid regex: …".
  Base.extend({
    kind: z.literal("search-scrollback"),
    request_id: z.string(),
    session_id: SessionId,
    grid_epoch: z.string(),
    query: z.string(),
    case_sensitive: z.boolean(),
    regex: z.boolean(),
    max_matches: z.number().int().positive(),
  }),
  // att1 file upload retired here — uploads stream via the DAttachmentChunk
  // worker-transport frame (coord AttachFileChunk RPC), not this JSON frame.
  // att2b — list a session's attachments. Worker returns entries
  // (filename, size_bytes, mtime_ms) for ~/.roost/attachments/<sid>/.
  Base.extend({
    kind: z.literal("list-attachments"),
    request_id: z.string(),
    session_id: SessionId,
  }),
  // att2b — delete a single attachment. Worker validates path stays
  // inside the session dir (no traversal).
  Base.extend({
    kind: z.literal("delete-attachment"),
    request_id: z.string(),
    session_id: SessionId,
    filename: z.string(),
  }),
  // diag — coord asks the worker to dump its in-memory byte ring for
  // the given session, capturing the last 256KB of PTY output bytes.
  // Worker writes ~/Library/Logs/RoostWorker/bytecap-<sid>-<ts>.bin
  // and returns the absolute path via rpc-ok { path: string }.
  // Triggered by SPA-side anomaly detectors via DiagSnapshot.
  Base.extend({
    kind: z.literal("diag-dump-bytecap"),
    request_id: z.string(),
    session_id: SessionId,
    reason: z.string(),
  }),
  // diag — coord asks worker for a snapshot of all in-memory state
  // for the diag.snapshot event. Worker returns rpc-ok with a
  // JSON-stringified payload.
  Base.extend({
    kind: z.literal("diag-snapshot"),
    request_id: z.string(),
  }),
  // Re-create a session at a specific session_id + cwd + kind iff the
  // worker doesn't currently hold it. No-op when the worker already
  // has the sid live (e.g. survivor keeper resumed across a worker
  // process restart). Coord fires this from worker-service.ts on
  // worker.hello for every coord DB row whose worker_fp matches the
  // freshly-attached worker AND status='open'. Closes the gap where a
  // keeper-protocol bump wipes the worker's in-memory sessions but the
  // coord DB still has the row + the SPA still shows the terminal.
  // Worker emits a fresh `opened` SessionEvent on respawn so coord
  // projector + SPA both pick up the new channel.
  Base.extend({
    kind: z.literal("respawn-if-missing"),
    request_id: z.string(),
    session_id: SessionId,
    cwd: z.string(),
    cols: z.number().int().positive().default(80),
    rows: z.number().int().positive().default(24),
  }),
]);
export type ClientControlFrame = z.infer<typeof ClientControlFrame>;

// ─── binary frame header (PTY bytes) ───────────────────────────────────
// Binary WS messages from worker carry: 2-byte BE channel_id + 1-byte
// direction tag (0=from-pty, 1=to-pty) + raw bytes. JSON above carries
// everything else.
export const DIR_FROM_PTY = 0;
export const DIR_TO_PTY = 1;
// phase-ssb2: FROM_PTY frames now carry [2 ch][1 dir=0][8 BE end_seq][bytes].
// end_seq = worker's per-session head_seq AFTER appending this chunk.
// phase-ssb7: DIR_SCROLLBACK_MARK retired — splice ordering is per-byte
// now, no in-band sentinel needed.
