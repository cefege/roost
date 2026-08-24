// Module-level constants + pure helpers for SessionManager. Split out of
// session-manager.ts (400-line cap); values/behavior byte-for-byte unchanged.

import { createHash } from "node:crypto";
import { homedir } from "node:os";
import { join } from "node:path";
import { createWtermCore } from "@roost/shared/wterm-core-factory";

// Hex8 of sha256(bytes). Cheap content-fingerprint for the diagnostic stream.
export function _sha8(bytes: Uint8Array | Buffer): string {
	return createHash("sha256").update(bytes).digest("hex").slice(0, 8);
}

// OPT2-2: the grid is built by the SHARED createWtermCore (loads the same
// roost-patched wasm the SPA does — MAX_SCROLLBACK_LINES 1k→10k, phase-pb9b
// — so a serialized alt-screen snapshot carries full 10k-line depth). Coord
// reuses the same factory for its status-scrape grid; see
// @roost/shared/wterm-core-factory.
export const _createWtermCore = createWtermCore;

// phase-ssb-altmode: DEC private mode escape sequences for alt-screen.
// Longest sequence we look for is 8 bytes; carry 7 from chunk N to
// chunk N+1 so a mode-set straddling the boundary still matches.
export const MODE_CARRY_MAX = 7;

// Phase-3: cell-delta coalesce window. ~one frame at 60fps — collapses a burst
// of PtyOut chunks into a single latest-state delta. Imperceptible echo latency
// on a local/LAN worker; bounds frame rate under floods.
export const CELL_EMIT_COALESCE_MS = 16;
// Coordinator-only raw terminal metadata uses this same leading/trailing
// cadence. These caps bound the brief worker-side staging queues; the encoded
// CoordLink outbox has its own independent 8 MiB cap.
export const RAW_METADATA_CHANNEL_CAP_BYTES = 256 * 1024;
export const RAW_METADATA_AGGREGATE_CAP_BYTES = 2 * 1024 * 1024;

// Live PTY bytes staged across session-resume()'s multi-await adoption window
// (history read + WASM core build can span seconds while the keeper keeps
// producing). Same magnitude as RAW_METADATA_CHANNEL_CAP_BYTES: enough for a
// burst, small against a flood. Overflow FAILS ADOPTION rather than dropping:
// a PTY stream is contiguous, so discarding either end splices an invisible
// byte hole into parser state — the respawn path is the only recovery that
// keeps the no-gap invariant.
export const RESUME_STAGE_CAP_BYTES = 256 * 1024;

// DEC private mode 2026 (synchronized output). An application that opens a
// synchronized frame is telling the renderer not to paint a half-drawn grid, so
// the emitter withholds streaming cell frames until the frame closes. A stream
// that opens one and never closes it — a TUI killed mid-repaint, a truncated
// recording, a `printf` that emitted only the opener — would otherwise withhold
// FOREVER: the browser goes dark while the core keeps parsing. Two independent
// ceilings, because the two stuck shapes are different.
//
// Wall ceiling: a stuck block that goes SILENT produces no further chunks, so
// nothing re-evaluates it; only an armed timer can recover that one. One second
// is the plan's upstream recovery ceiling — long enough that a legitimate
// multi-chunk repaint completes inside it, short enough that a user reads it as
// a hitch rather than a hang.
//
// It bounds the EMITTER's own withholding and nothing else: a resize
// transaction installs its own cell-emission gate, whose bound is the
// transaction's phase budget, and it retires any open hold on the way in
// (session-resize-capture.ts::installResizeCapture) precisely so the two
// ceilings never compose into a hang neither one admits to.
export const SYNC_OUTPUT_MAX_MS = 1_000;
// Work ceiling: rows the browser may fall behind inside ONE synchronized frame,
// counted as scrollback lines appended since the hold opened plus the currently
// dirty viewport rows. A stuck block that keeps FLOODING is caught here, well
// before the wall ceiling. 2,000 rows is ~83 full 80x24 repaints, more than one
// second of 60fps full-screen redraw, so a real synchronized frame never
// approaches it.
export const SYNC_OUTPUT_MAX_PENDING_ROWS = 2_000;




// Reverse-reap sweep: every N ms diff the keeper's live channels against
// this.sessions (the worker's authoritative tracked set) and SIGKILL strays.
// A stray = a keeper PTY the worker no longer owns (a deleted session whose
// KillChild no-op'd on a channel-mismatched keeper, or a channel left over from
// a prior keeper generation). Without this nothing ever kills the survivor and
// coord's open rows drift far below the live PTY count (12 rows / 88 processes).
export const STRAY_REAP_INTERVAL_MS = 60_000;

// A channel must read stray for this many CONSECUTIVE sweeps before it's reaped.
// Guards the spawn window: this.sessions.set trails pool.spawn by a beat, so a
// just-spawned channel is briefly in the keeper but not yet tracked — one grace
// interval covers it, two strikes never kills a live spawn.
export const STRAY_REAP_STRIKES = 2;

// keeper.degraded: ≥N emit_no_session within the window → the survivor keeper
// is emitting on dead channels (births dead PTYs). Tuned to not fire on a
// single mid-kill race but to catch a sustained degraded keeper fast.
export const KEEPER_DEGRADED_WINDOW_MS = 30_000;
export const KEEPER_DEGRADED_THRESHOLD = 5;

// keeper dead-birth self-heal: a child that exits within DEAD_BIRTH_LIFETIME_MS
// of spawn having produced ZERO bytes (head_seq===0) is stillborn — the same
// degraded-keeper class, caught from the close side (spawn → instant exit) in
// addition to emit_no_session. ≥THRESHOLD within the window → restart the
// keeper. head_seq===0 is the discriminator: a real shell prints a prompt
// (≥1 byte) before exiting, so a legit fast `exit` is NOT counted.
export const DEAD_BIRTH_LIFETIME_MS = 2_000;
export const KEEPER_DEAD_BIRTH_THRESHOLD = 3;

// A channel emits a few PTY bytes (prompt epilogue / exit message) in the
// window AFTER the worker deleted its SessionRecord — the keeper is a separate
// process, so in-flight PtyOut frames arrive post-close. These tail emits are
// benign (bytes correctly dropped) but were counting toward _noSessionBurst and
// re-tripping keeper.degraded right after a reconcile → restart LOOP that
// SIGTERMs every live PTY (CLAUDE.md keeper-degradation memory, mechanism
// proven 2026-06-23). A channel still emitting PAST this TTL is a TRUE orphan
// (degraded keeper driving a dead channel) and DOES count.
export const RECENTLY_CLOSED_TTL_MS = 750;
