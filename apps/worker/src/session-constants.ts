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

// 8 MB sliding scrollback window kept on the worker per session.
// Matches the keeper's ring (sb30) so getScrollback can serve a fresh
// SPA the same depth of history the keeper started us with. ~24 KB
// of memory overhead is rounding error for the saved roundtrip.
// 2026-06-22: 8 MB → 1 MB, matched to KEEPER_RING_CAP_BYTES (multiplexed-main.ts).
// Smaller per-channel footprint on a permanently RAM-full box; ~10k lines is
// ample. See memory project_keeper_death_auto_respawn.
export const SCROLLBACK_CAP_BYTES = 1 * 1024 * 1024;

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



// Multi-viewer PTY size: SCD (smallest-common-denominator) policy. PTY
// size = min(cols) × min(rows) across all active viewer claims so no
// viewer is ever clipped. Decision lives in _recomputeViewport. SCD min
// was briefly retired for WINDOW_SIZE_LATEST in phase-pathl (6cad2d71)
// but LATEST let a refresh/focus hijack everyone's size AND oscillated
// under a now-fixed accidental-reactive-dep bug
// ([[feedback_viewport_claim_accidental_grid_dep]]); SCD restored
// 2026-06-18 as a stable fixed point. SPA mirrors the same min in
// CellTerminal.tsx so wterm grid === PTY grid. claimViewport bumps lastMs
// only on focus/input (heartbeats refresh presence without bumping).
// TTL + withdraw-grace are shared with coord (@roost/shared/viewport) so
// the two sides' claim sets can't desync — see that file for the why.
export const VIEWPORT_REAPER_INTERVAL_MS = 5_000;

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
