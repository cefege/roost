// CellTerminal — cell-grid model terminal pane (R11, cell mode). Renders OUTPUT via
// CellGridRenderer (pre-rendered cells, never reflows → no history corruption)
// and handles INPUT via a HIDDEN @wterm/dom instance used ONLY as a keystroke
// encoder + terminal-mode oracle.
//
// Why the hidden wterm: keystroke→bytes encoding is mode-dependent — arrows
// (DECCKM ESC[?1h), bracketed paste (ESC[?2004h), keypad — and those modes are
// set by OUTPUT bytes. We feed the byte stream into the hidden wterm so its
// onData encodes correctly in vim/claude/htop, and reuse RoostTerm's
// L11-debugged focus dance (forceFocus blur-first + FocusEvent + mousedown
// recapture). The hidden wterm's own grid reflows internally but is NEVER
// shown — only its onData + mode state are used. Display = cells.
//
// The ONLY terminal renderer (byte mode / Terminal.tsx deleted 2026-06-23 — cell
// is canonical). Ports that rode the byte stream live here: ghost cursors
// (cellRenderer.setGhosts), OSC 8 links (Osc8Tracker + attachTerminalLinks).
// Input rides inputChannel.sendInput (bytes).

import {
	onMount,
	onCleanup,
	createEffect,
	createMemo,
	createSignal,
	Show,
	getOwner,
	runWithOwner,
	on,
} from "solid-js";
import { RoostTerm } from "../lib/RoostTerm.ts";
import { CellGridRenderer } from "../lib/cellRenderer.ts";
import { createScrollbackBackfill } from "../lib/scrollbackBackfill.ts";
import { PredictiveEcho } from "../lib/predictiveEcho.ts";
import { liveStatus } from "../lib/attention.ts";
import { AgentLaunchButton } from "./AgentLaunchButton.tsx";
import { AttachFileButton } from "./AttachFileButton.tsx";
import { PlanButton } from "./PlanButton.tsx";
import { TerminalContextMenu } from "./TerminalContextMenu.tsx";
import { pickAndAttachFiles, enqueueAttachment } from "../lib/attachments.ts";
import { MobileVoiceInput, activeVoiceChannel } from "./MobileVoiceInput.tsx";
import { TerminalNavButtons } from "./TerminalNavButtons.tsx";
import { TerminalStatusBadge } from "./TerminalStatusBadge.tsx";
import { mouseForwardEnabled } from "../lib/mouseForwardPref.ts";
import { isCompact, isTouchDevice } from "../lib/windowSizeClass.ts";
import { micOnDesktop } from "../lib/micOnDesktop.ts";
import { keyboardOnDesktop } from "../lib/keyboardOnDesktop.ts";
import { Osc8Tracker } from "./terminal-osc8.ts";
import { attachTerminalLinks, type ResolveFile } from "./terminal-links.ts";
import { downloadWorkerFileByHref } from "../lib/downloadWorkerFile.ts";
import { registerRenderer } from "../lib/terminalPreview.ts";
import {
	registerBytesHandler,
	registerCellHandler,
	registerPresenceHandler,
} from "../store/sync.ts";
import { inputChannel, consumeLastInputSendTs } from "../ws/input-channel.ts";
import {
	recordInput,
	getInputText,
	clearInput,
} from "../lib/terminalInputHistory.ts";
import { coordClient } from "../connect.ts";
import { ResizeCause } from "@roost/shared/proto/coordinator_pb";
import { isResizeDragging, arrangeEpoch } from "../lib/resizeDrag.ts";
import { diag, signal } from "@roost/shared/diag";
import { springStep, isSpringAtRest, SPRING_STIFF, type SpringState } from "../lib/spring.ts";
import { getSessionTraceId } from "../lib/diag.ts";
import type { Session } from "@roost/shared/wire";
import { useNavigate } from "@solidjs/router";
import { createOfflineWatch } from "../lib/offlineWatch.ts";
import { pageVisible, isPageVisible } from "../lib/pageVisible.ts";
import { syncStreamOpen } from "../store/sync-stream-open.ts";
import { newestOpenSessionForFolderKey } from "../store/selectors.ts";
import { folderKeyOf } from "../lib/folderKey.ts";
import { TerminalOfflineNotice } from "./TerminalOfflineNotice.tsx";
import { isPendingSpawn } from "../store/optimisticSpawn.ts";

interface CellTerminalProps {
	session: Session;
	// In the current tiling layout (a visible pane's selected tab) → claim size +
	// render cells. Not in layout → withdraw + go dormant (stays MOUNTED for the
	// persistent-deck no-remount guarantee). Replaces the old single `isActive`.
	inLayout?: boolean;
	// Owns the keyboard = the focused pane's selected tab. Only the focused pane
	// force-focuses and runs the document focus-recovery nets, so N live panes
	// don't fight for the keyboard.
	focused?: boolean;
	// The floated pane's selected tab (spotlight). Flipping this is an
	// intent-bearing resize → force an exact re-fit (see effect in onMount).
	spotlit?: boolean;
}

const CLAIM_DEBOUNCE_MS = 150;
// Liveness heartbeat for the viewport claim. The worker reaps a claim after
// VIEWER_CLAIM_TTL_MS (120s, viewport.ts) of silence and then STOPS emitting
// cells to that viewer (_hasActiveViewer gate, session-manager.ts). A
// foreground tab left idle — no resize, no visibility flip; e.g. the machine
// slept — sends no claim, so after 120s the pane goes dead-on-type: input
// reaches the worker and updates the grid, but no cells come back until a
// refresh/tab-switch re-claims. 30s = ¼ TTL, matching the "viewers refresh
// every 30s" the worker's reaper comment already assumes.
const CLAIM_HEARTBEAT_MS = 30_000;

// Reveal re-pin burst: after a hidden→visible re-claim snapshot paints, the
// content-visibility scrollback blocks reflow from their contain-intrinsic-size
// estimate to real height over the next few frames, moving the true bottom. Re-
// assert scrollToBottom across this window instead of trusting one synchronous
// atBottom() read (which sees the pre-reflow estimate). ~12 frames ≈ 200ms @60fps.
const REPIN_SETTLE_FRAMES = 12;

// A finger must travel this far before a touch counts as a scroll (vs a tap-to-
// focus). Below it, follow intent is left untouched so tapping never breaks it.
const TOUCH_SCROLL_SLOP_PX = 8;
// Keep auto-pin suppressed this long after the finger lifts so iOS/Android fling
// momentum settles before follow re-syncs to the resting position.
const TOUCH_MOMENTUM_GRACE_MS = 300;

// Shared 500ms cursor-poll ticker — one interval for ALL mounted panes (was one
// per open session; the deck keeps every open session mounted). Ref-counted like
// StatusGlyph's spinner tick: starts on first register, stops when the last pane
// unregisters. Per-instance gating (inLayout/visible/changed) stays in each
// callback.
const CURSOR_POLL_MS = 500;
const _cursorPollCbs = new Set<() => void>();
let _cursorPollHandle: number | null = null;
function _registerCursorPoll(cb: () => void): () => void {
	_cursorPollCbs.add(cb);
	if (_cursorPollHandle === null) {
		_cursorPollHandle = window.setInterval(() => {
			for (const f of _cursorPollCbs) f();
		}, CURSOR_POLL_MS);
	}
	return () => {
		_cursorPollCbs.delete(cb);
		if (_cursorPollCbs.size === 0 && _cursorPollHandle !== null) {
			clearInterval(_cursorPollHandle);
			_cursorPollHandle = null;
		}
	};
}

// Grace before a viewed-but-frameless pane is declared "not responding" (a dead
// breadcrumb: open session, no live PTY). Applied PER ATTEMPT: on expiry the
// watch silently re-claims (up to 2×, one grace apart) before showing the
// notice, so a truly dead breadcrumb surfaces only after ~OFFLINE_GRACE_MS × 3
// (~9s) while a live-but-transiently-stuck pane self-heals without ever showing
// it. Long enough that a slow first snapshot never false-flags a live pane; the
// notice self-clears the instant a frame lands. See lib/offlineWatch.ts.
const OFFLINE_GRACE_MS = 3000;

// Elements that legitimately own the keyboard / pointer — a click or keystroke
// landing inside one must NOT be yanked back to the terminal: real text widgets,
// open overlays (rename dialog / ⌘K palette / context menu), and the terminal
// itself (its own click + selection are fine). Everything else (bare buttons:
// sidebar ✕/FAB, mic, nav-keys, launch FAB, tabs, toasts) is fair game to keep
// terminal focus. Shared by BOTH focus guards below so the allowlist can't drift.
const FOCUS_OWNERS =
	'input, textarea, select, [contenteditable=""], [contenteditable="true"], [role="textbox"], [role="searchbox"], [role="dialog"], [role="menu"], dialog, .wterm';

export function CellTerminal(props: CellTerminalProps) {
	// Resolve a file path from terminal output → internal file-viewer href.
	// Absolute paths pass through; relative resolve against the session cwd;
	// ~/ paths derive the home dir from cwd (e.g. /Users/you/Code/foo →
	// /Users/you). Feeds clickable file links (Cmd-click → FileViewerSheet).
	const resolveFile: ResolveFile = (rawPath, line) => {
		const cwd = props.session.cwd;
		let abs: string;
		if (rawPath.startsWith("/")) abs = rawPath;
		else if (rawPath.startsWith("~/")) {
			// Derive home dir from cwd (e.g. /Users/you/Code/foo → /Users/you).
			if (!cwd) return null;
			const parts = cwd.split("/");
			if (parts.length < 3) return null;
			abs = "/" + parts[1] + "/" + parts[2] + rawPath.slice(1);
		}
		else if (!cwd) return null;
		else abs = cwd.replace(/\/+$/, "") + "/" + rawPath.replace(/^\.\//, "");
		const enc = abs
			.split("/")
			.map((s) => (s ? encodeURIComponent(s) : s))
			.join("/");
		return `/file/${props.session.worker_fp}/${enc.replace(/^\//, "")}${line ? `#L${line}` : ""}`;
	};

	let displayRef: HTMLDivElement | undefined; // visible — CellGridRenderer paints here
	let inputHostRef: HTMLDivElement | undefined; // hidden — RoostTerm (input + mode oracle)

	let renderer: CellGridRenderer | null = null;
	let predictor: PredictiveEcho | null = null;
	let term: RoostTerm | null = null;
	let cellW = 0;
	let cellH = 0;
	let claimSeq = 0;
	let lastClaimed = { cols: 0, rows: 0 }; // last ADOPTED claim — hold-anchor for VIEWPORT wobble
	let resizeObs: ResizeObserver | null = null;
	let claimTimer: ReturnType<typeof setTimeout> | null = null;
	// Last claim/withdraw actually sent — dedups the double-withdraw: pane hide
	// withdraws, then the off-screen park's ResizeObserver tick routes through
	// sendClaim → sees inLayout=false → would withdraw AGAIN. Reset on claim.
	let _lastSent: "claim" | "withdraw" | null = null;
	let unmounted = false;
// Stick-to-bottom intent captured across withdrawal. The per-frame wasBottom
// read (cellRenderer.atBottom) is unreliable right after a pane is revealed:
// parking a pane off-screen reverts its content-visibility scrollback blocks
// (sidebar.css .cell-block) to placeholder sizes, corrupting scrollHeight/
// scrollTop, so atBottom() misreads false and the if(wasBottom) re-pin is
// skipped. _following is maintained only while the pane is live+visible
// (cell handler + scroll listener), so it holds the trustworthy pre-withdrawal
// intent; _repinPending forces a re-pin on the first frame(s) after reveal.
let _following = true;     // user stuck to bottom? (default: a fresh pane follows)
let _repinPending = false; // a reveal happened while _following → re-pin on next frame(s)
let _settling = false;              // true while the reveal re-pin burst is scrolling — self-inflicted scrolls, ignore them
let _settleRaf: number | null = null;
let _settleFrames = 0;
let _jumping = false; // true while the smooth jump-to-bottom animation runs — ignore the scroll listener
let _touchScrolling = false; // finger actively dragging a main-screen (non-alt) pane → suppress auto-pin
let _touchStartY = 0;        // clientY at touchstart, to measure drag distance vs slop
let _touchMoved = false;     // this touch has passed the slop → it's a scroll, not a tap
let _touchGraceTimer: ReturnType<typeof setTimeout> | null = null;
	const [altScreen, setAltScreen] = createSignal(false); // tracks frame.altScreen — gates mouse/touch forwarding + wheel passivity
	// Show the jump-to-bottom FAB once the user has scrolled up more than one
	// full viewport from the bottom (i.e. scrolling back manually would be tedious).
	const [showJumpDown, setShowJumpDown] = createSignal(false);
	// Leaving/entering alt-screen must re-evaluate the FAB (a TUI opening while
	// scrolled up must hide it). Reading altScreen() makes this reactive.
	createEffect(() => { altScreen(); updateJumpDownVis(); });

	// Show the launch-Claude FAB only at a plain shell prompt. Dead-simple
	// signal: the last non-blank viewport line ends in a shell sigil
	// ($ / % / # / ❯ / ➜ / »). claude/vim/htop bottom lines never do, so the
	// FAB vanishes the moment a TUI takes over.
	const SHELL_PROMPT_RE = /[$%#❯➜»λ›]\s*$/;
	const [atShellPrompt, setAtShellPrompt] = createSignal(false);

	// True while this session is an optimistic placeholder (spawn RPC in flight).
	// For a non-optimistic session this is always false → every gate below is a
	// no-op, so mount behaviour is byte-identical to before.
	const pending = createMemo(() => isPendingSpawn(props.session.id));

	// ── liveness: does this VIEWED pane actually receive frames? ──────────
	// A live pane paints a snapshot within a beat of being claimed; a dead
	// breadcrumb never does. offlineWatch turns that silent-blank state into an
	// explicit notice. hasFrame flips true on the first frame (below) and never
	// back — a session that ever painted is proven live.
	const navigate = useNavigate();
	const [hasFrame, setHasFrame] = createSignal(false);
	const [offline, setOffline] = createSignal(false);
	const retryOffline = () =>
		sendClaim(ResizeCause.TAB_VISIBLE);
	const offlineWatch = createOfflineWatch(OFFLINE_GRACE_MS, setOffline, () => {
		diag("cell.offline_retry", { sid: props.session.id });
		retryOffline();
	});
	createEffect(() =>
		offlineWatch.update(props.inLayout !== false && pageVisible(), hasFrame()),
	);
	onCleanup(() => offlineWatch.dispose());
	const offlineSibling = () =>
		newestOpenSessionForFolderKey(folderKeyOf(props.session), props.session.id);
	const openOfflineSibling = () => {
		const sib = offlineSibling();
		if (sib) navigate(`/s/${sib.id}`);
	};

	// Measure one monospace cell in the display font (probe — independent of
	// rendered content, so a claim can fire before the first frame arrives).
	function measureCell(): boolean {
		if (!displayRef) return false;
		const probe = document.createElement("span");
		probe.className = "cell-row";
		probe.style.position = "absolute";
		probe.style.visibility = "hidden";
		probe.style.whiteSpace = "pre";
		probe.textContent = "0".repeat(10);
		displayRef.appendChild(probe);
		const rect = probe.getBoundingClientRect();
		displayRef.removeChild(probe);
		if (rect.width === 0 || rect.height === 0) return false;
		cellW = rect.width / 10;
		cellH = rect.height;
		return true;
	}

	// Phase-4 (G9, the ignore-inactive-client-size policy): a BACKGROUND tab must not drag the
	// SCD-min PTY size for the foreground viewers. Withdraw this viewer's claim
	// (cols=0/rows=0 → worker withdrawViewport) when hidden; re-claim when
	// visible. The byte renderer (Terminal.tsx) already did this; cell mode (the
	// default) didn't, so a hidden cell tab pinned everyone to its size for the
	// 70s claim-freshness window. Visibility reads route through isPageVisible()
	// (lib/pageVisible.ts) so the __smoke.forceVisible automation pin applies.
	function sendWithdraw(): void {
		if (_lastSent === "withdraw") return; // already withdrawn — skip the duplicate RPC
		_lastSent = "withdraw";
		lastClaimed = { cols: 0, rows: 0 }; // re-show must re-claim, not be held by a stale anchor
		claimSeq += 1;
		diag("cell.claim", {
			sid: props.session.id,
			cols: 0,
			rows: 0,
			client_seq: claimSeq,
		});
		void coordClient.sessionsResize({
			sessionId: props.session.id,
			cols: 0,
			rows: 0,
			clientSeq: BigInt(claimSeq),
			cause: ResizeCause.WITHDRAW,
		});
	}

	// cause = the browser event behind this claim (ResizeCause model).
	// Worker hint only; defaults to VIEWPORT (a plain ResizeObserver tick).
	function sendClaim(cause: ResizeCause = ResizeCause.VIEWPORT): void {
		if (!displayRef || unmounted) return;
		if (pending()) return; // placeholder has no PTY yet — don't fire a doomed round-trip
		// Hidden tab → withdraw instead of claim (don't pin the SCD-min).
		if (!isPageVisible()) {
			sendWithdraw();
			return;
		}
		// B (draw only to attached clients): a non-active deck pane is mounted
		// but visibility:hidden — withdraw so the worker stops emitting cells to it.
		// Switching back re-claims (INITIAL/TAB_VISIBLE) → snapshot repaints. Without
		// this every tab claims ALL open sessions → nothing is ever "unwatched" and
		// the worker emit gate never fires.
		if (props.inLayout === false) {
			sendWithdraw();
			return;
		}
		if (cellW === 0 || cellH === 0) {
			if (!measureCell()) return;
		}
		const cs = getComputedStyle(displayRef);
		const padL = parseFloat(cs.paddingLeft) || 0;
		const padR = parseFloat(cs.paddingRight) || 0;
		const padT = parseFloat(cs.paddingTop) || 0;
		const padB = parseFloat(cs.paddingBottom) || 0;
		const usableW = Math.max(0, displayRef.clientWidth - padL - padR);
		const usableH = Math.max(0, displayRef.clientHeight - padT - padB);
		let cols = Math.max(1, Math.floor(usableW / cellW));
		let rows = Math.max(1, Math.floor(usableH / cellH));
		// Cell mode never reflows; a sub-grid-metric wobble can't corrupt scrollback.
		// Suppress no-change claims to avoid needless resize round-trips.
		if (cause === ResizeCause.VIEWPORT && lastClaimed.cols > 0) {
			if (cols === lastClaimed.cols && rows === lastClaimed.rows) return;
		}
		lastClaimed = { cols, rows };
		_lastSent = "claim";
		claimSeq += 1;
		diag("cell.claim", {
			sid: props.session.id,
			cols,
			rows,
			client_seq: claimSeq,
		});
		void coordClient.sessionsResize({
			sessionId: props.session.id,
			cols,
			rows,
			clientSeq: BigInt(claimSeq),
			cause,
		});
	}

	function scheduleClaim(cause: ResizeCause = ResizeCause.VIEWPORT): void {
		if (claimTimer) clearTimeout(claimTimer);
		claimTimer = setTimeout(() => {
			claimTimer = null;
			sendClaim(cause);
		}, CLAIM_DEBOUNCE_MS);
	}

	// Intent-bearing claims (in↔out-of-layout flip, visibilitychange, pageshow)
	// skip the 150ms viewport-wobble debounce — routing them through
	// scheduleClaim added a hard +150ms to every session switch before the
	// worker even started streaming. Clears any pending debounced claim so the
	// immediate send can't be followed by a stale double-send. ResizeObserver
	// claims keep the debounce (that's what it exists for — viewport wobble).
	function sendClaimNow(cause: ResizeCause): void {
		if (claimTimer) {
			clearTimeout(claimTimer);
			claimTimer = null;
		}
		sendClaim(cause);
	}

	// Drive the reveal re-pin across the reflow window. Idempotent restart: a new
	// snapshot frame during the burst just resets the frame counter (extends the
	// follow); the running rAF loop is not double-scheduled.
	function startRepinSettle(): void {
		_settleFrames = 0;
		_settling = true;
		if (_settleRaf != null) return; // loop already running
		const step = () => {
			if (unmounted || !renderer || !_repinPending) { _settling = false; _settleRaf = null; return; }
			renderer.scrollToBottom();
			if (++_settleFrames >= REPIN_SETTLE_FRAMES) {
				_repinPending = false;
				_settling = false;
				_settleRaf = null;
				return;
			}
			_settleRaf = requestAnimationFrame(step);
		};
		_settleRaf = requestAnimationFrame(step);
	}

	// One viewport-height of slack: below this the plain scroll is trivial, so the
	// FAB would just be noise. altScreen has no scrollback → never show.
	function updateJumpDownVis(): void {
		if (!displayRef || _jumping || altScreen()) { setShowJumpDown(false); return; }
		const fromBottom = displayRef.scrollHeight - displayRef.scrollTop - displayRef.clientHeight;
		setShowJumpDown(fromBottom > displayRef.clientHeight);
	}

	// Spring-driven scroll to the true bottom, then an instant snap to pin. The
	// target is recomputed each frame so content-visibility block reflow
	// (scrollHeight growth as off-screen .cell-block blocks reveal) can't leave
	// us landing short; the spring naturally chases the moving target. Shares the
	// app motion vocabulary (lib/spring.ts) instead of a bespoke easeOutCubic.
	function jumpToBottom(): void {
		if (!displayRef || !renderer) return;
		setShowJumpDown(false);
		_following = true;
		// Respect reduced motion — jump instantly.
		if (window.matchMedia?.("(prefers-reduced-motion: reduce)").matches) {
			renderer.scrollToBottom();
			return;
		}
		_jumping = true;
		const el = displayRef;
		let state: SpringState = { position: el.scrollTop, velocity: 0 };
		let last = performance.now();
		const t0 = last;
		const step = (now: number) => {
			if (unmounted || !renderer) { _jumping = false; return; }
			const dtMs = Math.min(now - last, 64); // clamp after a stall
			last = now;
			const target = el.scrollHeight - el.clientHeight;
			state = springStep(state, target, SPRING_STIFF, dtMs);
			// Hand off to the normal follow/pin machinery once at rest, within a
			// line of the true bottom, or after a wall-clock cap. The cap + the
			// near-bottom check are load-bearing: while output is STREAMING the
			// target grows every frame, so the spring never reaches rest — without
			// a bound the rAF would spin forever with _jumping stuck true.
			const nearBottom = target - state.position < 24; // ~one row of slack
			if (isSpringAtRest(state, target) || nearBottom || now - t0 > 600) {
				renderer.scrollToBottom(); // instant final snap → follow takes over
				_following = true;
				_jumping = false;
			} else {
				el.scrollTop = state.position;
				requestAnimationFrame(step);
			}
		};
		requestAnimationFrame(step);
	}

	onMount(async () => {
		try {
		inputChannel.start();
		const sid = props.session.id; // body-scope capture for onCleanup (L11)
		// Owner captured BEFORE the `await term.init()` below. onCleanup after an
		// await registers against a null owner (Solid drops it) → the teardown block
		// never runs → document drop/paste/mousedown/keydown listeners + intervals
		// leak per unmount. Leaked stale `onDrop` closures pass their isActive guard
		// (disposed memo's last value) → N× enqueueAttachment on one drop. Bind the
		// post-await onCleanup to this owner so teardown actually fires on unmount.
		const cellOwner = getOwner();

		// ── output: cells ────────────────────────────────────────────────
		renderer = new CellGridRenderer(displayRef!);
		const unregPreview = registerRenderer(props.session.id, renderer);
		// Lazy-history backfill: full frames carry only a scrollback tail
		// (sbBase); the controller pulls the rest per-viewer after the attach
		// settles (or immediately on scroll-up) and prepends it seam-free.
		const backfill = createScrollbackBackfill({
			sessionId: props.session.id,
			renderer: () => renderer,
		});
		const onBackfillScroll = () => {
			// Gate to visible+in-layout: a parked pane's content-visibility blocks
			// revert to placeholders and can fire spurious scroll events whose
			// atBottom() misreads would corrupt _following / arm backfill pointlessly.
			if (_settling || _jumping || props.inLayout === false || !isPageVisible() || !renderer) return;
			const atB = renderer.atBottom();
			_following = atB;
			if (!atB) backfill.onUserScrollUp();
			updateJumpDownVis();
		};
		displayRef!.addEventListener("scroll", onBackfillScroll, { passive: true });
		// Predictive local echo — speculative client overlay, gated on
		// SRTT + alt-screen; no-op on a fast link. See lib/predictiveEcho.ts.
		predictor = new PredictiveEcho(renderer.predictionHost, {
			sid: props.session.id,
			onCursor: (col) => renderer?.setPredictedCursor(col),
		});
		// Debug bridge (Step-1 diagnostic lever): behind the existing smoke flag so
		// production `window` stays clean. The last-mounted pane re-binds the
		// global, so focus the pane under test; on dispose, `predictor = null`
		// makes the closure return null. Returns counts/SRTT/epoch/mode only — no
		// session content.
		if (typeof localStorage !== "undefined" && localStorage.getItem("roostSmoke") === "1") {
			(window as Window & { __roostPredictDebug?: () => unknown }).__roostPredictDebug =
				() => predictor?._debug() ?? null;
		}
		// Ghost cursors: this viewer's latest cursor (sent to others) + the map of
		// OTHER viewers' cursors (received via presence, painted by cellRenderer).
		let lastCurRow = 0,
			lastCurCol = 0;
		const ghostMap = new Map<
			string,
			{ x: number; y: number; label?: string }
		>();
		const unsubCell = registerCellHandler(props.session.id, (frame) => {
			setHasFrame(true);
			// Echo RTT tracker: input→cell-frame round-trip, works even when
			// predictive echo is off. Consumes the last-send timestamp (one
			// measurement per input→echo cycle).
			const sendTs = consumeLastInputSendTs(props.session.id);
			if (sendTs !== undefined) {
				const rttMs = performance.now() - sendTs;
				if (rttMs > 0 && rttMs < 5000) diag("echo.frame_rtt", { sid: props.session.id, rtt_ms: rttMs });
			}
			if (!renderer) return;
			diag("cell.apply", {
				sid: props.session.id,
				seq: frame.seq,
				full: frame.full,
				vp_rows: frame.viewportRows.length,
				cursor_vis: frame.cursorVisible,
				cursor_row: frame.cursorRow,
				cursor_col: frame.cursorCol,
			});
			setAltScreen(frame.altScreen);
			const wasBottom = renderer.atBottom();
			renderer.apply(frame);
			renderer.setGhosts(ghostMap); // re-attach after the viewport re-render
			lastCurRow = frame.cursorRow;
			lastCurCol = frame.cursorCol;
			predictor?.onFrame(frame); // reconcile predictions against the authoritative grid
			// Only a LIVE (in-layout + page-visible) pane updates the stick-to-bottom
			// intent. A frame arriving while parked (in-flight after a withdraw, or a
			// late emit) reads wasBottom=false on the reverted content-visibility
			// container — writing that to _following would erase the pre-withdrawal
			// intent, so the reveal never arms _repinPending and never re-pins. The
			// scroll listener is already gated the same way (spurious parked scrolls).
			if (props.inLayout !== false && isPageVisible()) {
				const pin = (wasBottom && !_touchScrolling) || _repinPending;
				if (pin) renderer.scrollToBottom();
				// A reveal re-pin (_repinPending) must survive the post-paint content-
				// visibility reflow: drive scrollToBottom across a bounded rAF burst
				// that clears _repinPending when it lands, instead of a same-tick
				// atBottom() read against the pre-reflow (estimate) scrollHeight.
				if (_repinPending) startRepinSettle();
				if (!_touchScrolling) _following = pin;
				updateJumpDownVis();
			}
			if (frame.full) backfill.onFullFrame();
			setAtShellPrompt(SHELL_PROMPT_RE.test(renderer.viewportTail()));
		});

		// Receive remote viewers' cursors → ghostMap → cellRenderer (ch/lh overlay).
		const unsubPresence = registerPresenceHandler(props.session.id, (msg) => {
			const f = msg as {
				kind?: string;
				viewer_id?: string;
				cursor_col?: number;
				cursor_row?: number;
				label?: string;
			};
			if (f.kind === "presence-delta" && typeof f.viewer_id === "string") {
				ghostMap.set(f.viewer_id, {
					x: f.cursor_col ?? 0,
					y: f.cursor_row ?? 0,
					label: f.label ?? f.viewer_id,
				});
				renderer?.setGhosts(ghostMap);
			} else if (
				f.kind === "presence-leave" &&
				typeof f.viewer_id === "string"
			) {
				if (ghostMap.delete(f.viewer_id)) renderer?.setGhosts(ghostMap);
			}
		});

		// Send THIS viewer's cursor so others' ghostMaps update. Gated on visible +
		// active (a hidden deck pane has no one watching). 500ms, only on change.
		let lastSentRow = -1,
			lastSentCol = -1;
		const releaseCursorPoll = _registerCursorPoll(() => {
			if (props.inLayout === false || !isPageVisible()) return;
			if (lastCurRow === lastSentRow && lastCurCol === lastSentCol) return;
			lastSentRow = lastCurRow;
			lastSentCol = lastCurCol;
			void coordClient.sessionsCursorPos({
				sessionId: props.session.id,
				col: lastCurCol,
				row: lastCurRow,
			});
		});

		// Claim heartbeat: re-send the LAST claim VERBATIM (same dims, same
		// claimSeq → worker treats it as a no-op refresh: bumps lastMs, no
		// snapshot, no SIGWINCH, no flicker; session-manager.ts:1039 stale-seq
		// path) so the worker never reaps an active viewer mid-idle and stops
		// emitting cells. Gated active+visible — a hidden/inactive pane stays
		// withdrawn (sendClaim/onVisibility own that).
		const claimHeartbeat = setInterval(() => {
			if (props.inLayout === false || !isPageVisible()) return;
			if (lastClaimed.cols <= 0 || lastClaimed.rows <= 0) return;
			void coordClient.sessionsResize({
				sessionId: props.session.id,
				cols: lastClaimed.cols,
				rows: lastClaimed.rows,
				clientSeq: BigInt(claimSeq),
				cause: ResizeCause.VIEWPORT,
			});
		}, CLAIM_HEARTBEAT_MS);

		// ── input + mode oracle: hidden wterm ────────────────────────────
		// autoFocus off on touch — popping the on-screen keyboard on pane mount/
		// selection is unwanted; an explicit tap focuses (onDisplayClick).
		term = new RoostTerm(inputHostRef!, {
			autoFocus: props.focused === true && !isTouchDevice(),
			// Cells own the display; this wterm exists for mode state + onData
			// only. Renderless keeps its never-shown mirror grid out of the DOM
			// (~22k nodes/session otherwise — the focus-flip flush stall).
			renderless: true,
		});
		await term.init();
		if (unmounted) {
			term.destroy();
			return;
		}
		term.onData = (data: string) => {
			const bytes = new TextEncoder().encode(data);
			predictor?.predict(bytes); // speculative echo before the round-trip
			inputChannel.sendInput(props.session.id, bytes);
			recordInput(props.session.id, data); // typed text → keyterm context
		};
		// Feed the byte stream so DECCKM / bracketed-paste / keypad modes track
		// the running program → onData encodes arrows/paste correctly. The
		// hidden grid render is discarded (never shown).
		// OSC 8 hyperlink tracker — parses the byte stream for ESC]8;;URI links
		// (claude / ls --hyperlink) so attachTerminalLinks can wrap their visible
		// text in <a>. Same stream that feeds the hidden input oracle. Client-side,
		// no wire change (the cell wire carries no link metadata).
		const osc8 = new Osc8Tracker();
		const unsubBytes = registerBytesHandler(props.session.id, (chunk) => {
			const bytes =
				typeof chunk === "string" ? new TextEncoder().encode(chunk) : chunk;
			osc8.process(bytes);
			if (!term) return;
			term.write(bytes);
		});
		// Linkify rendered .cell-row text (OSC 8 links + regex URLs), Cmd/Ctrl-gated.
		const detachLinks = attachTerminalLinks(displayRef!, osc8, {
			resolveFile,
			// Cmd/Ctrl-click a file path → download it from the worker (works whether
			// the session is local or on another Mac), replacing the in-app viewer.
			onOpenFile: (href) => void downloadWorkerFileByHref(href),
			githubOwnerRepo: () => props.session.git_remote ?? undefined,
			// Freeze cell repaints while Cmd-hovering a link so the wrapped <a> stops
			// churning under the cursor (the pointer↔text flicker).
			onArmedHoverChange: (active) => renderer?.setArmedHold(active),
		});

		// MOBILE keyboard fix: tapping a sidebar row to switch terminals closes the
		// overlay, and that same tap's click falls through onto the now-revealed
		// display → onDisplayClick → forceFocus → on-screen keyboard pops, unwanted.
		// Record when this pane becomes active; on touch, the display handlers skip
		// focus within NAV_FALLTHROUGH_MS of activation (the fall-through) but a
		// deliberate later tap still focuses + opens the keyboard. Transition-only
		// (prevActive) so a re-running effect can't keep refreshing the window.
		const NAV_FALLTHROUGH_MS = 700;
		let lastActivatedMs = 0;
		let prevActive = false;
		createEffect(() => {
			const active = props.focused === true;
			if (active && !prevActive) lastActivatedMs = Date.now();
			prevActive = active;
		});
		const isNavFallthrough = () =>
			isTouchDevice() && Date.now() - lastActivatedMs < NAV_FALLTHROUGH_MS;

		// Clicks land on visible cell rows, not the off-screen textarea — bridge
		// them to the hidden wterm's focus (RoostTerm's own bridge is on the
		// hidden host, which never receives these clicks).
		const onDisplayDown = (ev: MouseEvent) => {
			// Left-click only: right-click (button 2) opens the context menu and
			// must NOT steal focus — forceFocus collapses the window selection, and
			// its microtask runs before `contextmenu` reads getSelection(), so the
			// "Copy selection" item would never see the marked text.
			if (ev.button !== 0) return;
			if (isNavFallthrough()) return;
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			queueMicrotask(() => term?.forceFocus());
		};
		displayRef!.addEventListener("mousedown", onDisplayDown);

		// CAN'T-TYPE-AFTER-CLICK fix: a REAL (trusted) left-click moves native focus
		// to <body> (the cell spans aren't focusable) AFTER the mousedown-microtask
		// forceFocus above → it overrides us, the textarea never holds focus, and
		// every keystroke vanishes (mic + Claude button still work — they're
		// programmatic, not keyboard). Synthetic events skip native focus, which is
		// why tests passed. The 'click' event fires AFTER native focus settles, so
		// re-grab focus there — unless the user just selected text (preserve it for
		// copy).
		const onDisplayClick = (ev: MouseEvent) => {
			if (ev.button !== 0) return;
			if (isNavFallthrough()) return; // sidebar-tap fall-through on mobile, not a real tap
			const t = ev.target as HTMLElement | null;
			if (t?.closest("button, input, textarea, a")) return;
			const sel = displayRef?.ownerDocument.getSelection();
			if (sel && !sel.isCollapsed) return; // text selected → don't steal focus
			term?.forceFocus();
		};
		displayRef!.addEventListener("click", onDisplayClick);

		// SELECTABLE-IN-ALT-SCREEN: the CellGridRenderer rebuilds the viewport DOM
		// (replaceChildren) on every frame, so a live TUI's constant repaints wiped
		// any in-progress text selection — selection was effectively impossible in
		// claude/vim/htop. While a non-collapsed selection sits over this pane,
		// freeze the renderer's DOM writes (it keeps folding frames into state);
		// releasing the selection (click/collapse/type) flushes the latest frame.
		// selectionchange is document-global → each pane checks its own displayRef.
		const onSelectionChange = () => {
			const sel = displayRef?.ownerDocument.getSelection();
			const held =
				!!sel &&
				!sel.isCollapsed &&
				sel.rangeCount > 0 &&
				!!sel.anchorNode &&
				displayRef!.contains(sel.anchorNode) &&
				!!sel.focusNode &&
				displayRef!.contains(sel.focusNode);
			renderer?.setSelectionHold(held);
		};

		// ── mouse / touch forwarding (SGR-1006 mouse-tracking) ───────────────
		// Ported from byte-mode phase-pb7c. Forwards pointer + touch gestures to
		// the TUI as mouse events so claude fullscreen scroll / click / drag work
		// in-app (nothing in @wterm/dom forwards the mouse — InputHandler is
		// keyboard-only). Gate: mouseForwardEnabled() (the nav-pad toggle) AND
		// altScreen — a plain shell keeps native browser selection + DOM scroll.
		// Shift/Alt held → bypass (the documented "force native select" escape
		// hatch, code.claude.com/docs/en/fullscreen). Cb bits: 0-1 button,
		// 2 shift, 3 meta, 4 ctrl, 5 motion(+32); wheel = 64 up / 65 down.
		const forwardActive = () => mouseForwardEnabled() && altScreen();
		const modifierBypass = (ev: MouseEvent | WheelEvent) =>
			ev.shiftKey || ev.altKey;
		const sendSeq = (seq: string) =>
			inputChannel.sendInput(props.session.id, new TextEncoder().encode(seq));
		const cellOf = (
			clientX: number,
			clientY: number,
		): { col: number; row: number } => {
			if ((cellW === 0 || cellH === 0) && !measureCell())
				return { col: 1, row: 1 };
			const rect = displayRef!.getBoundingClientRect();
			return {
				col: Math.max(1, 1 + Math.floor((clientX - rect.left) / cellW)),
				row: Math.max(1, 1 + Math.floor((clientY - rect.top) / cellH)),
			};
		};

		let pressedButton: number | null = null;
		let lastMotionCell: { col: number; row: number } | null = null;

		const onWheelForward = (ev: WheelEvent) => {
			if (!forwardActive() || modifierBypass(ev)) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			sendSeq(`\x1b[<${ev.deltaY < 0 ? 64 : 65};${col};${row}M`);
			ev.preventDefault();
		};
		const onMouseDownFwd = (ev: MouseEvent) => {
			if (!forwardActive() || modifierBypass(ev)) return;
			// Cmd/Ctrl-click on a linkified <a> (terminal-links.ts, armed to
			// pointer-events:auto only while the mod key is held) opens in THIS
			// browser via the native anchor. Do NOT also forward the click to the
			// worker's PTY — Claude in alt-screen would run `open <url>` and pop the
			// tab on the WORKER Mac's screen (single-click-opens-on-worker bug). Same
			// anchor-skip the focus handlers (onDisplayDown/onDisplayClick) use.
			if ((ev.target as HTMLElement | null)?.closest("a")) return;
			// Middle button is reserved for the deck's bring-to-front toggle
			// (TerminalDeck onDeckPointerDown) — never forwarded as SGR press.
			if (ev.button === 1) return;
			const button = ev.button === 1 ? 1 : ev.button === 2 ? 2 : 0;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			pressedButton = button;
			lastMotionCell = { col, row };
			sendSeq(
				`\x1b[<${button | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}M`,
			);
		};
		const onMouseMoveFwd = (ev: MouseEvent) => {
			if (pressedButton === null || !forwardActive()) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			if (
				lastMotionCell &&
				lastMotionCell.col === col &&
				lastMotionCell.row === row
			)
				return;
			lastMotionCell = { col, row };
			sendSeq(
				`\x1b[<${pressedButton | 32 | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}M`,
			);
		};
		const onMouseUpFwd = (ev: MouseEvent) => {
			if (pressedButton === null) return;
			const button = pressedButton;
			pressedButton = null;
			lastMotionCell = null;
			if (!forwardActive()) return;
			const { col, row } = cellOf(ev.clientX, ev.clientY);
			sendSeq(
				`\x1b[<${button | (ev.metaKey ? 8 : 0) | (ev.ctrlKey ? 16 : 0)};${col};${row}m`,
			);
		};

		// Touch: a finger-swipe → wheel events (the phone scroll path). One wheel
		// notch per cell-height of finger travel (the original speed — felt right);
		// drag DOWN = scroll toward history (wheel up 64), drag UP = toward latest
		// (wheel down 65). Only when forwarding is active — otherwise native
		// touch-scroll of the DOM stands. NO per-move cap — a long/fast swipe must
		// keep scrolling (capping made it "stop moving" mid-flick); the while-loop
		// drains the whole delta and carries the sub-notch remainder. (If claude
		// scrolls too few lines per notch, raise CLAUDE_CODE_SCROLL_SPEED in the
		// keeper env — the worker-side multiplier per the fullscreen docs.)
		let touchY: number | null = null;
		let touchCol = 1,
			touchRow = 1;
		const onTouchStart = (ev: TouchEvent) => {
			if (!altScreen() && ev.touches.length === 1) {
				_touchStartY = ev.touches[0]!.clientY;
				_touchMoved = false;
				if (_touchGraceTimer) { clearTimeout(_touchGraceTimer); _touchGraceTimer = null; }
			}
			if (!forwardActive() || ev.touches.length !== 1) {
				touchY = null;
				return;
			}
			const t = ev.touches[0]!;
			touchY = t.clientY;
			const c = cellOf(t.clientX, t.clientY);
			touchCol = c.col;
			touchRow = c.row;
		};
		const onTouchMove = (ev: TouchEvent) => {
			if (!altScreen()) {
				if (!_touchMoved && ev.touches.length === 1 &&
					Math.abs(ev.touches[0]!.clientY - _touchStartY) > TOUCH_SCROLL_SLOP_PX) {
					_touchMoved = true;
					_touchScrolling = true;
					_following = false; // user is scrolling into history → stop following
				}
				return; // native scroll owns the movement; never preventDefault outside alt-screen
			}
			if (touchY === null || !forwardActive() || ev.touches.length !== 1)
				return;
			const y = ev.touches[0]!.clientY;
			const step = cellH || 18; // original per-cell-height speed
			let dy = y - touchY;
			while (Math.abs(dy) >= step) {
				const up = dy > 0; // finger moved down → scroll up (history)
				sendSeq(`\x1b[<${up ? 64 : 65};${touchCol};${touchRow}M`);
				dy -= up ? step : -step;
			}
			touchY = y - dy; // carry sub-notch remainder
			ev.preventDefault(); // suppress native scroll only while forwarding
		};
		const onTouchEnd = () => {
			touchY = null;
			if (_touchMoved) {
				_touchMoved = false;
				clearTimeout(_touchGraceTimer ?? undefined);
				_touchGraceTimer = setTimeout(() => {
					_touchGraceTimer = null;
					_touchScrolling = false;
					// Re-sync follow intent to where the fling actually landed.
					if (renderer && props.inLayout !== false && isPageVisible()) {
						_following = renderer.atBottom();
						updateJumpDownVis();
					}
				}, TOUCH_MOMENTUM_GRACE_MS);
			}
		};

		displayRef!.addEventListener("mousedown", onMouseDownFwd);
		displayRef!.addEventListener("touchstart", onTouchStart, { passive: true });
		displayRef!.addEventListener("touchend", onTouchEnd, { passive: true });
		displayRef!.addEventListener("touchcancel", onTouchEnd, { passive: true });

		// att1d — file drop/paste → upload → inject abs_path into the PTY.
		// Lost in the byte-mode cut (e8f450b9); restored here. Logic lives in
		// attachments.ts::enqueueAttachment (same path as the touch picker).
		//
		// Listen on DOCUMENT (not just the cell grid): in cell mode the grid is
		// letterboxed (narrower than the pane), so a drop on the surrounding margin
		// missed the grid handler → the browser opened the image in a new tab.
		// The ACTIVE+visible pane owns the drop for the whole window; guarded to
		// file drags only so text/selection drags aren't hijacked.
		const dragHasFiles = (e: DragEvent) =>
			!!e.dataTransfer && Array.from(e.dataTransfer.types).includes("Files");
		const onDragOver = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault(); // allow the drop + stop the browser opening the file
		};
		const onDrop = (e: DragEvent) => {
			if (!props.focused || !isPageVisible() || !dragHasFiles(e)) return;
			e.preventDefault();
			const items = e.dataTransfer?.items;
			if (!items) return;
			for (let i = 0; i < items.length; i++) {
				if (items[i]!.kind === "file") {
					const f = items[i]!.getAsFile();
					if (f) void enqueueAttachment(props.session, f);
				}
			}
		};
		const onPaste = (e: ClipboardEvent) => {
			const items = e.clipboardData?.items;
			if (!items) return;
			for (let i = 0; i < items.length; i++) {
				if (items[i]!.kind === "file") {
					const f = items[i]!.getAsFile();
					if (f) void enqueueAttachment(props.session, f);
				}
			}
		};
		displayRef!.addEventListener("paste", onPaste);

		// Focus the hidden textarea whenever this pane is the active one. NON-
		// deferred (mirrors Terminal.tsx ~L945) so it fires on mount too: the prior
		// { defer: true } variant skipped the initial run, so a pane that mounts
		// already-active never got focused → focus stuck on <body> → every keystroke
		// dropped. THIS was the cell-phase-3b "can't input in cell mode" bug. Created
		// inside onMount so `term` is already assigned when it first runs.
		// Claim gate: every pane IN the layout claims size + gets cells; a pane that
		// leaves the layout withdraws so the worker stops emitting to it (re-claim +
		// snapshot on return). Hidden panes stay MOUNTED (no remount — persistent
		// deck) but go dormant. Multiple panes claim at once now (tiling) — each is a
		// distinct session/PTY, so no SCD self-clamp.
		// inLayout is derived from a per-layout-commit slot object (TerminalDeck
		// slotBySession → new ref every focus/close) — reading props.inLayout raw
		// re-runs this effect on EVERY layout mutation → a spurious TAB_VISIBLE
		// re-claim → full snapshot repaint of every terminal at once. Memo dedupes
		// on the boolean value so we re-claim/withdraw ONLY on a real in↔out flip.
		// on(): the callback body is non-tracking by design — sendClaimNow runs
		// sendClaim SYNCHRONOUSLY inside this effect (unlike the old scheduleClaim
		// timer, whose sendClaim ran outside tracking), so its reactive reads
		// (pending(), raw props.inLayout at the withdraw gate) would otherwise
		// join this effect's deps → re-run per slot rect change → band-bypassing
		// TAB_VISIBLE claim per deck resize. on() keys re-runs to the flag alone.
		const inLayoutFlag = createMemo(() => props.inLayout);
		createEffect(on(inLayoutFlag, (flag) => {
			if (!flag) {
				sendWithdraw();
				return;
			}
			_repinPending = _following; // park corrupted scroll state; re-pin on the re-claim snapshot
			sendClaimNow(ResizeCause.TAB_VISIBLE);
		}));
		// Focus gate: only the FOCUSED pane's terminal grabs the keyboard. Touch
		// devices skip it (an explicit tap on the display still focuses) so selecting
		// a pane doesn't pop the on-screen keyboard. Cold-mount focus (textarea not
		// yet in DOM while WASM init runs) is covered by RoostTerm's autoFocus
		// (fires forceFocus after init, see the constructor above) — this effect
		// handles the become-focused-later flips, when init is long done.
		const focusGate = createMemo(
			() => props.inLayout === true && props.focused === true,
		);
		createEffect(() => {
			if (focusGate() && !isTouchDevice())
				queueMicrotask(() => term?.forceFocus());
		});

		// Per-pane GLOBAL listeners (window/document) attach only while this pane
		// is IN the layout: the deck keeps every open session mounted, so the old
		// unconditional attach scaled N handlers per pointer/selection/drag event
		// app-wide. runWithOwner(cellOwner): post-await effects have no owner, and
		// this one MUST dispose on unmount or the listeners leak. Cleanup closes
		// over locals only (L11 feedback_no_props_read_in_oncleanup).
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				if (!inLayoutFlag()) return;
				document.addEventListener("selectionchange", onSelectionChange);
				window.addEventListener("mousemove", onMouseMoveFwd);
				window.addEventListener("mouseup", onMouseUpFwd);
				document.addEventListener("dragenter", onDragOver);
				document.addEventListener("dragover", onDragOver);
				document.addEventListener("drop", onDrop);
				onCleanup(() => {
					document.removeEventListener("selectionchange", onSelectionChange);
					window.removeEventListener("mousemove", onMouseMoveFwd);
					window.removeEventListener("mouseup", onMouseUpFwd);
					document.removeEventListener("dragenter", onDragOver);
					document.removeEventListener("dragover", onDragOver);
					document.removeEventListener("drop", onDrop);
				});
			}),
		);

		// Wheel/touchmove passivity: preventDefault (mouse-forwarding) only ever
		// fires in alt-screen — outside it the always-non-passive listeners
		// disabled compositor fast-scroll on every terminal. Swap on altScreen
		// flips. Remove-then-add matters: `passive` isn't part of listener
		// identity, so re-adding the same fn without removing first is silently
		// ignored as a duplicate (the effect's onCleanup runs before each re-run).
		runWithOwner(cellOwner, () =>
			createEffect(() => {
				const passive = !altScreen();
				displayRef!.addEventListener("wheel", onWheelForward, { passive });
				displayRef!.addEventListener("touchmove", onTouchMove, { passive });
				onCleanup(() => {
					displayRef?.removeEventListener("wheel", onWheelForward);
					displayRef?.removeEventListener("touchmove", onTouchMove);
				});
			}),
		);

		// ── viewport claim — drives PTY/grid size; worker emits a full cell
		//    snapshot on claim so the pane paints immediately. sendClaim()
		//    self-measures on first call (cellW/cellH still 0). ──────────────
		// Fire the INITIAL claim once the session is real. Non-optimistic: now
		// (pending already false) — identical timing to the old unconditional call.
		// Optimistic: when the spawn confirms and pending flips false. Bare
		// createEffect post-await matches the inLayout/focus effects above (tracks
		// reactively, re-runs on the flip); initialClaimSent guarantees exactly one.
		let initialClaimSent = false;
		createEffect(() => {
			// Don't fire until the deck has measured and assigned a slot (inLayout) AND
			// any optimistic spawn has resolved (pending). Before that, a claim would
			// either withdraw (inLayout=false) or be dropped (pending=true), consuming
			// initialClaimSent and preventing re-fire when both conditions are met.
			if (pending() || props.inLayout !== true) return;
			if (initialClaimSent) return;
			initialClaimSent = true;
			sendClaim(ResizeCause.INITIAL);
		});
		resizeObs = new ResizeObserver(() => {
			if (isResizeDragging()) return; // suppress mid-drag PTY round-trips; flush on release (effect below)
			scheduleClaim(ResizeCause.VIEWPORT);
		});
		resizeObs.observe(displayRef!);

		// Divider/sidebar drag: claims are gated off during the drag (resizeObs
		// above). The final size == the last drag frame's size, so the observer
		// does NOT re-fire after commit — fire the single settle claim here on
		// release.
		let wasResizeDragging = false;
		createEffect(() => {
			const dragging = isResizeDragging();
			if (dragging) {
				if (claimTimer) { clearTimeout(claimTimer); claimTimer = null; }
			} else if (wasResizeDragging && props.inLayout !== false && isPageVisible()) {
				sendClaim(ResizeCause.VIEWPORT);
			}
			wasResizeDragging = dragging;
		});

		// Spotlight float/push-back is an INTENT-bearing resize: the pane's slot
		// jumps to the card rect (or back to its tile). Nothing else fires a settle
		// claim for it (the passive ResizeObserver claim doesn't re-fit the PTY on
		// this jump), so force one on the flip. rAF: measure AFTER the slot's new
		// size has laid out.
		const spotlitFlag = createMemo(() => !!props.spotlit);
		let wasSpotlit = false;
		createEffect(() => {
			const s = spotlitFlag();
			if (s === wasSpotlit) return;
			wasSpotlit = s;
			requestAnimationFrame(() => {
				if (props.inLayout !== false && isPageVisible()) {
					sendClaim(ResizeCause.VIEWPORT);
				}
			});
		});

		// Arrange presets (TerminalDeck doArrange) re-tile every pane at once.
		// Same settle as spotlight above: rAF (commitLayout applies the new slot
		// rects reactively AFTER doArrange returns — a synchronous measure here
		// would read the STALE pre-arrange size). sendClaim no-ops when this
		// pane's size didn't change, so unaffected panes cost nothing; panes newly
		// ENTERING the layout already re-claim exact via the inLayout TAB_VISIBLE
		// effect, and panes LEAVING are gated out by the inLayout check. defer:
		// the INITIAL claim covers mount.
		createEffect(on(arrangeEpoch, () => {
			requestAnimationFrame(() => {
				if (props.inLayout !== false && isPageVisible()) {
					sendClaim(ResizeCause.VIEWPORT);
				}
			});
		}, { defer: true }));

		// G9: withdraw on hide, re-claim on show — a background tab must not pin
		// the SCD-min size for foreground viewers.
		const onVisibility = () => {
			if (isPageVisible()) { _repinPending = _following; sendClaimNow(ResizeCause.TAB_VISIBLE); }
			else sendWithdraw();
		};
		document.addEventListener("visibilitychange", onVisibility);
		// Re-claim whenever the firehose WS (re)opens. Cell content is live-forward
		// only (not in sinceEventId backfill), so a reconnect drops the grid stale
		// until the worker re-emits a snapshot — which only claimViewport triggers.
		// The visibility-regain re-claim races the WS close (registered first), so
		// its frame is lost; this is the deterministic safety net that lands the
		// snapshot on the live new socket.
		createEffect(on(syncStreamOpen, (open) => {
			if (!open || props.inLayout === false || !isPageVisible()) return;
			_repinPending = _following; // WS reconnect re-emits a snapshot; re-pin if was following
			sendClaimNow(ResizeCause.TAB_VISIBLE);
		}, { defer: true }));

		// Hard-exit release: tab close / reload / cross-page nav never runs
		// onCleanup, so without this the worker holds the claim until the TTL
		// reaper and other viewers stay clamped to this tab's size. Best-effort
		// unary (connect-web can't set fetch keepalive) — if the browser cancels
		// it mid-flight, the worker-side freshness tick recovers at
		// VIEWER_CLAIM_FRESH_MS. pageshow re-claims after a bfcache restore
		// (visibilitychange doesn't always fire on restore).
		const onPageHide = () => {
			if (props.inLayout !== false) sendWithdraw();
		};
		const onPageShow = () => {
			if (isPageVisible() && props.inLayout !== false) {
				_repinPending = _following;
				sendClaimNow(ResizeCause.TAB_VISIBLE);
			}
		};
		window.addEventListener("pagehide", onPageHide);
		window.addEventListener("pageshow", onPageShow);

		// ── FOCUS SAFETY NET (the "sometimes can't type, refresh/switch fixes it"
		//    bug) ──────────────────────────────────────────────────────────────
		// wterm's textarea lives offscreen; clicking any bare control (sidebar
		// ✕/FAB, mic, nav-keys, launch FAB, tabs) parks focus ON the control →
		// onData stops firing → keystrokes vanish until remount/pane-switch. Two
		// capture-phase nets on this active+visible pane keep that from happening:
		//
		// (1) mousedown PREVENT — a click on anything outside FOCUS_OWNERS keeps
		//     focus on the terminal (preventDefault blocks the focus-shift; the
		//     click still fires, so the button works). This is the primary fix —
		//     focus never leaves, so no keystroke is lost.
		const onDocMousedown = (e: MouseEvent) => {
			if (!props.focused || !isPageVisible()) return;
			if (e.button !== 0) return; // left only; right-click → context menu
			const t = e.target as HTMLElement | null;
			// Allow clicks inside ANY pane (this one or another) — clicking a pane must
			// be able to focus it. Only bare chrome OUTSIDE every pane keeps focus here.
			if (t?.closest(FOCUS_OWNERS) || t?.closest("[data-pane]")) return;
			e.preventDefault();
		};
		document.addEventListener("mousedown", onDocMousedown, true);
		//
		// (2) keydown RECOVER — backstop for focus an overlay grabbed
		//     programmatically (⌘K palette, rename dialog) and then dropped, where
		//     no mousedown fired. Recovers on a real typing key when focus is parked
		//     nowhere (body/html) or on a bare control; leaves text widgets/overlays
		//     and Space/Enter (button activation) alone. inputHostRef short-circuit
		//     keeps the per-keystroke hot path off the closest() walk.
		const onDocKeydown = (e: KeyboardEvent) => {
			if (!props.focused || !isPageVisible()) return;
			if (e.metaKey || e.ctrlKey || e.altKey) return;
			const ae = document.activeElement as HTMLElement | null;
			if (inputHostRef?.contains(ae)) return; // already typing into this terminal
			if (ae && ae !== document.body && ae !== document.documentElement) {
				if (ae.closest(FOCUS_OWNERS)) return;
				if (e.key.length !== 1 || e.key === " ") return;
			}
			diag("focus.recover", {
				sid: props.session.id,
				via: "keydown",
				key: e.key.length === 1 ? "char" : e.key,
			});
			term?.forceFocus();
		};
		document.addEventListener("keydown", onDocKeydown, true);

		runWithOwner(cellOwner, () =>
			onCleanup(() => {
				unsubCell();
				unsubBytes();
				unsubPresence();
				detachLinks();
				releaseCursorPoll();
				clearInterval(claimHeartbeat);
				document.removeEventListener("visibilitychange", onVisibility);
				window.removeEventListener("pagehide", onPageHide);
				window.removeEventListener("pageshow", onPageShow);
				document.removeEventListener("keydown", onDocKeydown, true);
				document.removeEventListener("mousedown", onDocMousedown, true);
				sendWithdraw(); // release this viewer's claim immediately on nav-away
				backfill.dispose();
				displayRef?.removeEventListener("scroll", onBackfillScroll);
				displayRef?.removeEventListener("mousedown", onDisplayDown);
				displayRef?.removeEventListener("click", onDisplayClick);
				displayRef?.removeEventListener("mousedown", onMouseDownFwd);
				displayRef?.removeEventListener("touchstart", onTouchStart);
				displayRef?.removeEventListener("touchend", onTouchEnd);
				displayRef?.removeEventListener("touchcancel", onTouchEnd);
				displayRef?.removeEventListener("paste", onPaste);
				resizeObs?.disconnect();
				if (claimTimer) clearTimeout(claimTimer);
				if (_settleRaf != null) { cancelAnimationFrame(_settleRaf); _settleRaf = null; }
				if (_touchGraceTimer) { clearTimeout(_touchGraceTimer); _touchGraceTimer = null; }
				predictor?.dispose();
				renderer?.dispose();
				unregPreview();
				term?.destroy();
				clearInput(sid); // drop the typed-text ring on real unmount
				predictor = null;
				renderer = null;
				term = null;
			}),
		);
		} catch (e) {
			// A renderer/term init throw here is an unhandled rejection = silent
			// dead pane (the "can't input" class). Surface it as a corruption signal.
			signal("diag.corruption_signal", {
				kind: "cell_mount_failed",
				sid: props.session.id,
				session_trace_id: getSessionTraceId(props.session.id),
				msg: String(e),
				cooldownKey: props.session.id,
			});
		}
	});

	onCleanup(() => {
		unmounted = true;
	});

	return (
		<div
			style={{
				position: "absolute",
				inset: "0",
				display: "flex",
				"min-height": "0",
			}}
		>
			<div
				ref={displayRef}
				style={{ flex: "1", "min-width": "0", "min-height": "0", "touch-action": "pan-y" }}
			/>
			{/* Optimistic spawn placeholder: paint the pane instantly; the real
          terminal reconciles into this same tab when the spawn RPC resolves. */}
			<Show when={pending()}>
				<div style={{ position: "absolute", inset: "0", display: "flex", "align-items": "center", "justify-content": "center", color: "var(--text-lo)", "font-size": "13px", "pointer-events": "none" }}>
					Starting…
				</div>
			</Show>
			{/* Off-screen, NOT aria-hidden: Chrome blocks programmatic focus on
          descendants of aria-hidden subtrees → the textarea must stay reachable
          or input dies. Hidden from sight via -99999px is enough. */}
			<div
				ref={inputHostRef}
				style={{
					position: "absolute",
					left: "-99999px",
					top: "0",
					width: "240px",
					height: "120px",
					overflow: "hidden",
				}}
			/>
			{/* Mic + on-screen keypad — always on touch/compact; on desktop each is
          gated by its own pref (mic / nav pad). Input rides inputChannel. */}
			<Show when={isCompact() || isTouchDevice() || micOnDesktop()}>
				<MobileVoiceInput
					channelId={props.session.channel}
					sendInput={(_ch, data) =>
						inputChannel.sendInput(props.session.id, data)
					}
					readContext={() => ({
						grid: renderer?.gridText() ?? "",
						scrollback: renderer?.scrollbackText() ?? "",
						input: getInputText(props.session.id),
					})}
					refocusTerminal={() => term?.forceFocus()}
				/>
			</Show>
			<Show when={isCompact() || isTouchDevice() || keyboardOnDesktop()}>
				<TerminalNavButtons sessionId={props.session.id} />
			</Show>
			{/* Launch-agent FAB — shells only, shown only at a plain shell prompt
          (regex on the live viewport tail) AND not while voice-recording
          (shares the discard-✕ slot; would cover the cancel button). Types the
          selected agent's command + CR into the PTY; agent configurable in
          Settings. */}
			<Show when={props.session.kind === "shell" && atShellPrompt() && activeVoiceChannel() === null}>
				<AgentLaunchButton sessionId={props.session.id} />
			</Show>
			{/* Plan-mode shortcut FAB — agent sessions only, shown when the agent is
          idle (at its prompt, ready for a command). Types '/plan' + CR into
          the PTY, entering plan mode. Mirrors the agent-launch button's
          sendInput path; shares its fixed slot (mutually exclusive: the
          agent-launch shows only on shells at a shell prompt). */}
			<Show when={liveStatus(props.session) === "idle" && activeVoiceChannel() === null}>
				<PlanButton sessionId={props.session.id} />
			</Show>
			{/* Attach-file FAB — always on. Native picker → chunked upload (progress
          chip) → abs_path injected into the PTY, same as drag-drop. */}
			<AttachFileButton session={props.session} />
			<Show when={!pending() && !offline() && props.inLayout !== false}>
				<TerminalStatusBadge session={props.session} />
			</Show>
			{/* Jump-to-latest FAB — bottom-center, shown only when scrolled up > 1
          viewport (never in alt-screen). Tap → fixed-duration eased scroll to
          the bottom + pin, resuming live-follow. */}
			<Show when={showJumpDown() && props.inLayout !== false}>
				<button
					type="button"
					class="jump-bottom-fab"
					data-testid="scroll-to-bottom"
					aria-label="Scroll to bottom"
					title="Scroll to bottom"
					onPointerDown={(e) => e.preventDefault()}
					onClick={jumpToBottom}
				>
					<span class="jump-bottom-fab__icon">arrow_downward</span>
				</button>
			</Show>
			<TerminalContextMenu
				session={props.session}
				getContainer={() => displayRef ?? null}
				onAttachFile={() => pickAndAttachFiles(props.session)}
			/>
			<Show when={offline()}>
				<TerminalOfflineNotice
					onRetry={retryOffline}
					onOpenSibling={openOfflineSibling}
					hasSibling={!!offlineSibling()}
				/>
			</Show>
		</div>
	);
}
