// Session lifecycle record. Split out of session-manager.ts and re-exported.
//
// Every session owns terminal state: scrollback ring, wterm core, and cell
// emitter.

import type { SessionId, ChannelId } from "@roost/shared/wire";
import type { FsmChannel } from "./fsm.ts";
import type { TerminalCore } from "@wterm/core";
import type { CellEmitState } from "@roost/shared/cell";
import type { PrStatus } from "./pr-status.ts";
import type { SbRing } from "./session-scrollback-ring.ts";
import type { AgentOscState } from "./terminal-stream-scan.ts";
import type { ShellSpec } from "./shell-spec.ts";
import type { LifecycleReservation } from "./event-sink.ts";

/** One distinct escape sequence the core's dispatcher did not recognise, as
 *  first seen on the CURRENT core instance. */
export interface UnhandledSequenceEntry {
	/** CSI final byte, e.g. "q" for DECSCUSR. */
	final: string;
	/** Private-parameter prefix ("?", ">", "<", "="), or "" when absent. */
	private: string;
	/** Parameters the core parsed; it only records the first four into `params`,
	 *  so this can exceed `params.length`. */
	paramCount: number;
	params: number[];
	/** Monotonic ms (util/mono.ts) at which this core first produced it. */
	firstSeenMonoMs: number;
}

/** Roost's view of ONE core instance's unhandled-sequence ring. The ring is
 *  never cleared, so `consumed` — the ring's own logged total as of the last
 *  sample — is the high-water mark that keeps a stale entry from being reported
 *  twice, and `keys` dedupes repeats of the same sequence. Bounded at the ring's
 *  capacity; session-unhandled-seq.ts owns every mutation of this shape. */
export interface UnhandledSequenceLog {
	/** Sequences the core had logged in total when last sampled, duplicates
	 *  included. Nothing at or below this index is ever reported again. */
	consumed: number;
	/** Distinct sequences observed, capped at UNHANDLED_SEQ_MAX. */
	entries: UnhandledSequenceEntry[];
	/** `sequenceKey` of every recorded entry. */
	keys: Set<string>;
	/** Entries the core's ring overwrote between two samples while this
	 *  accumulator was still recording, so Roost never saw them: only their
	 *  existence is knowable, not what they were. */
	ringDropped: number;
	/** `entries` reached the cap; later distinct sequences are not recorded. */
	capped: boolean;
}

interface SessionRecordCommon {
	sessionId: SessionId;
	channelId: ChannelId;
	socketPath: string;
	cwd: string;
	// Immutable launch contract retained for same-session keeper-loss respawn.
	// `cwd` may drift through OSC 7; shellSpec.cwd remains the spawn folder.
	shellSpec: ShellSpec;
	// Local git branch of cwd (worker-resolved). undefined = not yet resolved,
	// null = folder isn't a repo. Set by _startGitBranch; announced in
	// snapshots + pushed via the `git` SessionEvent. gitWatchDispose closes the
	// .git/HEAD fs.watch on close / cwd-change. See git-branch.ts.
	git_branch?: string | null;
	git_remote?: string | null; // github owner/repo of origin (worker-resolved)
	gitWatchDispose?: (() => void) | null;
	// GitHub PR status for git_branch, resolved via `gh pr list` (pr-status.ts).
	// Retained on the record so snapshots re-announce it across coord restart
	// (like git_branch). Pushed via the `pr` SessionEvent; polled every 90s.
	pr?: PrStatus | null;
	prPollTimer?: ReturnType<typeof setInterval> | null;
	// Keeper child pid — root of the pid-tree walk in listening-ports.ts.
	// Retained on the record so snapshots re-announce ports across coord restart.
	childPid?: number | null;
	ports?: number[];
	portsPollTimer?: ReturnType<typeof setInterval> | null;
	fsm: FsmChannel;
	// diag — stable per-session id used to correlate ALL events on this
	// session across spa+coord+worker via `rg session_trace_id`. Set on
	// session create; never mutated.
	session_trace_id: string;
	// Wall-clock at spawn. closedByKeeper checks (now - spawnedAtMs) against
	// DEAD_BIRTH_LIFETIME_MS: a child that exits fast having produced zero bytes
	// (head_seq===0) is a dead-birth → feeds the degraded-keeper self-heal.
	spawnedAtMs: number;
	// Capacity admitted before this session's keeper/core mutation. Exactly one
	// natural, requested, or reconciliation close consumes it.
	closeReservation: LifecycleReservation;
}

/** The last core-rebuild origin pin, and the history floor that pin established.
 *
 *  A rebuild is the one moment Roost's monotonic numbering is re-derived rather
 *  than merely advanced. A replacement core restarts its discarded counter at
 *  zero; a preserved alternate core retains it. `sbOrigin` absorbs either
 *  difference so browser-held absolute row indexes never re-alias. It is also
 *  the only moment history can vanish for a reason OTHER than eviction: a
 *  bounded replay can rebuild shallower history and move the floor.
 *
 *  One fixed record per session, overwritten in place by its single writer
 *  (rebuildTerminalCore), so sampling it is O(1) and it retains no history. */
export interface SbOriginPin {
	/** Monotonic ms at the pin. Age, never a wall clock, so a host clock step
	 *  cannot forge or hide how long ago the floor moved. */
	at_mono_ms: number;
	cols: number;
	rows: number;
	/** True when the full retained byte ring was replayed into a replacement
	 *  core; false when a frozen alternate core was caught up in place. */
	replayed_ring: boolean;
	/** The capture's recorded boundary had already fallen out of the ring. */
	ring_evicted: boolean;
	/** OLD core, read live at the pin: the floor and monotonic total the pin has
	 *  to reproduce. */
	prev_dropped: number;
	prev_total: number;
	/** Core counters after replay or in-place alternate resize. */
	fresh_discarded: number;
	fresh_count: number;
	/** The pin's outputs — the additive origin and the floor it establishes. */
	sb_origin: number;
	sb_dropped: number;
	/** `prev_total - fresh_discarded - fresh_count` went NEGATIVE and Math.max(0,…)
	 *  clamped it: the replay rebuilt MORE lines than the old core reported, so
	 *  numbering continuity was discarded instead of preserved. */
	clamped: boolean;
	/** Rows that existed under the old core's numbering and do not exist under the
	 *  fresh one: `max(0, sb_dropped - prev_dropped)`. History lost to the REPLAY
	 *  BOUND, not to eviction — a session never resized would still hold them. 0
	 *  when the rebuild preserved (or, with a deeper core, recovered) everything. */
	replay_lost_rows: number;
	/** Highest floor a replay bound has ever established here, carried across
	 *  pins. The floor only rises, so one comparison classifies the CURRENT floor:
	 *  still equal ⇒ the floor a caller just hit is this replay's; higher ⇒
	 *  ordinary eviction has since carried it past. */
	replay_floor: number;
}

export interface SessionShellRecord extends SessionRecordCommon, AgentOscState {
	kind: "shell";
	// Per-session sliding scrollback window (fixed-capacity byte ring). Appended
	// on every keeper output chunk alongside the live upstream forward; replayed
	// into a fresh wterm core when the grid is rebuilt at a new size, which is
	// what makes the rebuilt grid a pure function of (ring, cols, rows).
	scrollback: SbRing;
	// phase-ssb1: monotonic byte-offset seq for the END of `scrollback`.
	// First byte ever appended has logical seq 1; head_seq = total bytes
	// appended over session lifetime (NOT total bytes retained — ring may
	// have evicted). tail_seq = head_seq - scrollback.length, i.e. the
	// logical seq of the byte BEFORE scrollback[0].
	head_seq: number;
	// Alt-screen tracking. TUIs such as vim and less use DEC private mode
	// 1049 (or 47/1047) to swap to an off-scrollback buffer. The rebuilt core
	// must re-enter the same mode or redraws land on the wrong rows.
	// `mode_carry` preserves a transition split across chunks.
	alt_mode: boolean;
	mode_carry: Uint8Array;
	// OSC 7 cwd tracking. Shells emit
	// `ESC ] 7 ; file://host/percent-encoded-path BEL` when their directory
	// changes. `osc7_carry` holds the tail of a split sequence.
	osc7_carry: Uint8Array;
	// Capability-probe tokenizer carry (terminal-query-reply.ts). Holds the
	// unterminated CSI prefix a chunk ended on so a probe split across pty chunk
	// boundaries is recognised exactly once. Advanced by the capture lane too:
	// the frozen core never parses those bytes, but the stream did move, and a
	// partial glued onto a post-rebuild chunk would answer a probe nobody sent.
	query_carry: Uint8Array;
	// Agent-state fallback reads OSC title/progress directly from this same PTY
	// stream; see AgentOscState / initAgentOscState in terminal-stream-scan.ts.
	// @wterm/core WASM bridge that mirrors every PTY byte. Authoritative grid
	// the cell emitter reads and the resize rebuild replays the ring into.
	wtermCore: TerminalCore;
	// R11 cell-grid cell-shipping emitter state. Full/delta decision + seq live in
	// @roost/shared/cell::nextCellFrame.
	cell_emit: CellEmitState;
	// Arrival wall-clock of the OLDEST PTY byte not yet shipped in a cell frame.
	// 0 = nothing pending. emitCellFrame stamps PbCellFrame.ptyOutMs from it and
	// resets it, so __roostLag()'s worker_prep segment measures the real
	// keeper→coalesce→grid-read leg instead of collapsing to zero.
	lastPtyOutMs: number;
	// History-truth record of the last core rebuild (session-resize-capture.ts),
	// null until the first one. Read by the diagnostic snapshot and by the
	// get-scrollback-cells floor reason; NEVER by the emit path.
	sb_origin_pin: SbOriginPin | null;
	// Escape sequences THIS core instance reported as unhandled, plus the
	// high-water mark that stops the core's never-cleared ring from re-reporting
	// them. undefined = this core has logged nothing, the healthy case, in which
	// nothing is allocated at all. See session-unhandled-seq.ts.
	unhandled?: UnhandledSequenceLog;

}

export type SessionRecord = SessionShellRecord;
