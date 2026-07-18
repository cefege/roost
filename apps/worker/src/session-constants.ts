// Module-level constants + pure helpers for SessionManager. Split out of
// session-manager.ts (400-line cap); values/behavior byte-for-byte unchanged.

import { createHash } from "node:crypto";
import { join, basename } from "node:path";
import { createWtermCore } from "@roost/shared/wterm-core-factory";

// Hex8 of sha256(bytes). Cheap content-fingerprint for the diag stream
// so claude can correlate the same chunk across spa+coord+worker logs.
export function _sha8(bytes: Uint8Array | Buffer): string {
	return createHash("sha256").update(bytes).digest("hex").slice(0, 8);
}

// Hook exec = absolute bun + absolute script (source tree; rsync deploys
// preserve layout). launchd PATH is minimal and the hook script path may
// contain a space — both paths quoted, neither relies on PATH lookup.
const HOOK_SCRIPT = join(import.meta.dir, "cli", "hook.ts");
export const HOOK_CMD = basename(process.execPath) === "bun"
	? `"${process.execPath}" run "${HOOK_SCRIPT}"`
	: `"${process.execPath}" hook`;

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

// herdr agent-status detection. Debounce off the PTY byte path so a multi-chunk
// claude repaint is scraped once from the settled grid.
export const DETECT_DEBOUNCE_MS = 150;

// working→idle HOLD (herdr AGENT_PENDING_IDLE_CAP): don't commit idle the instant
// the spinner blinks off mid-turn; wait until the byte stream stays quiet this
// long. A visible blocker/idle screen bypasses the hold (arbiter handles it).
// 800ms was too short: claude pauses >1s mid-turn (thinking, waiting on a tool)
// and briefly shows the ✳ idle title → premature idle commit → running↔idle
// flap (measured 1085/1154/1785ms gaps). 3s absorbs those pauses; a genuinely
// finished turn still commits idle ~3s later, imperceptible for a sidebar chip.
// needs-input is NOT gated by this (blocker bypasses the hold) so prompts still
// light instantly.
export const AGENT_WORKING_GRACE_MS = 3000;

// herdr-style idle re-scan. Byte-driven detection only fires while an agent
// produces output, so an agent idle + quiet since a worker restart never gets
// scraped and shows no chip. A periodic sweep re-runs detection on every session
// so idle agents surface a status too (herdr scrapes on a timer). _runDetect
// dedups, so a steady session re-emits nothing — only the cheap scrape recurs.
// ponytail: O(sessions) grid-reads per tick; fine for a normal fleet, raise the
// interval if a very large fleet shows CPU.
export const DETECT_SWEEP_INTERVAL_MS = 4000;

// Last complete OSC 0/2 window-title in a raw chunk, UTF-8 intact — the wterm
// core's OSC parser is ASCII-only and strips claude's braille spinner, so we
// parse the title off the raw stream instead. Returns null if no complete title
// is present. ponytail: no cross-chunk carry — claude re-emits the title every
// frame, so a title split across a chunk boundary heals on the next repaint.
export function extractOscTitle(bytes: Uint8Array): string | null {
	let result: string | null = null;
	for (let i = 0; i + 1 < bytes.length; i++) {
		if (bytes[i] !== 0x1b || bytes[i + 1] !== 0x5d) continue; // ESC ]
		let j = i + 2;
		const psStart = j;
		while (j < bytes.length && bytes[j] !== 0x3b && j - psStart < 3) j++; // Ps then ';'
		if (bytes[j] !== 0x3b) continue;
		const ps = new TextDecoder().decode(bytes.subarray(psStart, j));
		if (ps !== "0" && ps !== "2") continue; // 0/2 = window title
		const titleStart = ++j;
		while (
			j < bytes.length &&
			bytes[j] !== 0x07 &&
			!(bytes[j] === 0x1b && bytes[j + 1] === 0x5c)
		)
			j++;
		if (j >= bytes.length) continue; // incomplete (spans chunks) — skip
		result = new TextDecoder().decode(bytes.subarray(titleStart, j));
		i = j;
	}
	return result;
}

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
