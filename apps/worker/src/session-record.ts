// Session lifecycle record + viewport-claim shapes. Split out of
// session-manager.ts (400-line cap); types unchanged, re-exported from there.

import type { SessionId, ChannelId } from "@roost/shared";
import type { FsmChannel } from "./fsm.ts";
import type { TerminalCore } from "@wterm/core";
import type { CellEmitState } from "@roost/shared/cell";

export type SessionRecord = {
	sessionId: SessionId;
	channelId: ChannelId;
	socketPath: string;
	kind: "shell" | "claude";
	cwd: string;
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
	pr?: import("./pr-status.ts").PrStatus | null;
	prPollTimer?: ReturnType<typeof setInterval> | null;
	// Keeper child pid — root of the pid-tree walk in listening-ports.ts.
	// Captured from pool.spawn's return. Retained on the record so snapshots
	// re-announce ports across coord restart. Ports pushed via `ports` event.
	childPid?: number | null;
	ports?: number[];
	portsPollTimer?: ReturnType<typeof setInterval> | null;
	fsm: FsmChannel;
	// Per-session sliding scrollback for SPA browser-refresh recovery.
	// Appended on every keeper output chunk (along with the live upstream
	// forward); served verbatim via sessions.getScrollback so a fresh SPA
	// mount restores the last 8 MB in a single mutation, no protocol
	// change to the keeper.
	scrollback: Uint8Array;
	// phase-ssb1: monotonic byte-offset seq for the END of `scrollback`.
	// First byte ever appended has logical seq 1; head_seq = total bytes
	// appended over session lifetime (NOT total bytes retained — ring may
	// have evicted). tail_seq = head_seq - scrollback.length, i.e. the
	// logical seq of the byte BEFORE scrollback[0]. getScrollbackSince
	// serves [lastSeq+1, head_seq] iff lastSeq >= tail_seq; otherwise
	// returns gap=true signalling the SPA that the ring has rolled past
	// its lastSeq and the session history is unrecoverable. Replaces the
	// sb59-sb63 splice-mark + 5s-timeout band-aid per
	// project_seqno_splice_path_a_chosen.md + CLAUDE.md L11.
	head_seq: number;
	// phase-ssb-altmode: alt-screen tracking. Claude/vim/less use DEC
	// private mode 1049 (or 47/1047) to swap to an off-scrollback buffer
	// and back. If the keeper's been running long enough that the ring
	// evicted the original "enter alt-screen" sequence, a fresh SPA mount
	// replays the snapshot in main-screen mode — every claude UI redraw
	// becomes scrollback noise + live cursor-positioned updates land on
	// the wrong rows. User-visible "two streams / wallpaper" symptom.
	// Workaround: scan every appended chunk for the DEC private mode
	// 1049/47/1047 transitions, track current state, prepend the right
	// enter sequence to getScrollbackSince output iff currently alt.
	// `mode_carry` keeps the trailing bytes of the previous chunk so a
	// mode-set sequence split across chunk boundaries still parses.
	alt_mode: boolean;
	mode_carry: Uint8Array;
	// 2026-06-15: OSC 7 cwd tracking. Plain shells (bash/zsh on macOS)
	// emit `ESC ] 7 ; file://host/percent-encoded-path BEL` on every
	// chpwd when TERM_PROGRAM=Apple_Terminal triggers the system zshrc /
	// bashrc hook (see keeper/main.ts ptyEnv). claude emits cwd patches
	// via its tool parser. Both routes land here as a `cwd` SessionEvent
	// → coord projector → session.cwd persists → sidebar group label
	// tracks `cd` across refresh and across browsers. `osc7_carry` holds
	// the tail of the previous chunk so a sequence split mid-byte still
	// matches on the next chunk.
	osc7_carry: Uint8Array;
	// @wterm/core WASM bridge that mirrors every PTY byte. Used for
	// getScrollbackSince(0) fresh-mount + gap replay: serializeWTerm()
	// walks the captured grid and emits ANSI that recreates the visible
	// viewport + scrollback at headless cols/rows. The SPA's wterm
	// writes that ANSI into the SAME parser code path — one VT engine
	// end-to-end, no cross-parser edge cases. Live deltas (lastSeq>0)
	// still slice the raw byte ring above (byte-exact, no parser cost).
	wtermCore: TerminalCore;
	// diag — stable per-session id used to correlate ALL events on this
	// session across spa+coord+worker via `rg session_trace_id`. Set on
	// session create; never mutated.
	session_trace_id: string;
	// R11 cell-grid cell-shipping emitter state. Full/delta decision + seq live in
	// @roost/shared/cell::nextCellFrame.
	cell_emit: CellEmitState;
	// Wall-clock at spawn. closedByKeeper checks (now - spawnedAtMs) against
	// DEAD_BIRTH_LIFETIME_MS: a child that exits fast having produced zero bytes
	// (head_seq===0) is a dead-birth → feeds the degraded-keeper self-heal.
	spawnedAtMs: number;
};

export interface ViewportClaim {
	cols: number;
	rows: number;
	lastMs: number;
	clientSeq?: number;
}
