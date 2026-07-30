// Global keydown listener installed once from App.tsx onMount.
// Routes keyboard shortcuts to open CommandPalette, HelpOverlay,
// plus ↑/↓/⏎ flat-sidebar list navigation. ⌘F is owned by AllView (sidebar filter).
// Owns the open/close signals for each modal; components import these signals.
//
// Callers: App.tsx (onMount / onCleanup). Signals imported by modal components.
// Depends on: solid-js createSignal + sidebarCursor.

import { createSignal } from "solid-js";
import { moveCursor, activateCursor } from "./sidebarCursor.ts";

/** True when the event target is a text-editable surface (input/textarea/
 *  contentEditable). xterm's PTY uses an off-screen textarea, so this guard
 *  keeps arrow/Enter keys flowing to the terminal when it's focused. */
function isEditableTarget(t: EventTarget | null): boolean {
	const el = t as HTMLElement | null;
	if (!el || !el.tagName) return false;
	const tag = el.tagName;
	return tag === "INPUT" || tag === "TEXTAREA" || el.isContentEditable === true;
}

/** True when a terminal deck is on screen. wterm's PTY textarea sits
 *  off-screen at left:-9729px and drops DOM focus routinely (the L11
 *  focus-drift saga) — so isEditableTarget alone lets ⏎ leak to the sidebar
 *  cursor while the user believes they're typing in the terminal, jumping
 *  them to another session/workspace.
 *
 *  BUG (post pane-tiling): the old guard keyed off `[data-pane][data-focused=
 *  "true"]` — the per-slot focus flag. That flag is absent during EVERY
 *  layout reconcile and the ResizeObserver 0×0 window (view() returns zero
 *  panes → zero slots → zero data-focused="true" elements), and it's only ever
 *  "true" on the pane's SELECTED tab. In those transient windows ⏎ fell
 *  through to activateCursor() → navigate to whatever row the (persisted)
 *  sidebar cursor last sat on → the intermittent "Enter jumps me to another
 *  workspace" boot loop, present even with a single pane.
 *
 *  Fix: key off the STABLE deck element `[data-testid="terminal-deck"]`
 *  (TerminalDeck.tsx) — mounted for the whole lifetime a terminal folder is
 *  active, independent of per-slot focus and the RO tick. If the deck is on
 *  screen, the terminal owns ↑/↓/⏎ regardless of document.activeElement. */
export function terminalOwnsKeyboard(): boolean {
	return (
		document.querySelector('[data-testid="terminal-deck"]') !== null ||
		document.querySelector("[data-pane]") !== null
	);
}

// ── CommandPalette (Cmd-K) ──────────────────────────────────────────────────
// Cmd-K toggles the session/action search palette. Folder browsing lives at
// /browse (BrowsePage.tsx), opened by the "+" entry points — the palette no
// longer carries a folder context.
const [_cmdPaletteOpen, _setCmdPaletteOpen] = createSignal(false);
export const cmdPaletteOpen = _cmdPaletteOpen;
export function openCmdPalette() {
	_setCmdPaletteOpen(true);
}
export function closeCmdPalette() {
	_setCmdPaletteOpen(false);
}

// ── HelpOverlay (Shift+?) ───────────────────────────────────────────────────
const [_helpOpen, _setHelpOpen] = createSignal(false);
export const helpOpen = _helpOpen;
function openHelp() {
	_setHelpOpen(true);
}
export function closeHelp() {
	_setHelpOpen(false);
}


// ── Global handler ─────────────────────────────────────────────────────────
// ⌘F is intentionally NOT handled here — it belongs to the in-place sidebar
// filter (AllView focuses its search input).

// Exported for the keyboard-nav regression test (keyboardShortcuts.dom.test.ts):
// drives a real KeyboardEvent through the exact production branch that decides
// whether ⏎ navigates the sidebar vs. defers to the terminal. Not called
// directly elsewhere — installKeyboardShortcuts wires it to window.
export function handleKeydown(e: KeyboardEvent): void {
	if (e.defaultPrevented) return;
	// window capture runs before wterm or CellTerminal can prevent the event.
	// Defer non-Meta keys from its hidden input, or from body/html while a deck
	// is visible, so terminal ownership can encode the original key first.
	const terminalInput = (e.target as HTMLElement | null)?.closest?.(".wterm");
	if (
		!e.metaKey &&
		(terminalInput ||
			(terminalOwnsKeyboard() &&
				(document.activeElement === document.body ||
					document.activeElement === document.documentElement)))
	)
		return;



	// Escape: close whichever modal is open (highest-z first).
	if (e.key === "Escape") {
		if (_cmdPaletteOpen()) {
			closeCmdPalette();
			return;
		}
		if (_helpOpen()) {
			closeHelp();
			return;
		}
		return;
	}

	// Cmd-K (macOS Meta) or Ctrl-K: toggle CommandPalette.
	if ((e.metaKey || e.ctrlKey) && e.key === "k") {
		e.preventDefault();
		if (_cmdPaletteOpen()) closeCmdPalette();
		else openCmdPalette();
		return;
	}


	// Shift+? : toggle HelpOverlay.
	if (e.shiftKey && e.key === "?") {
		// Avoid firing while typing in an input/textarea.
		const tag = (e.target as HTMLElement).tagName;
		if (tag === "INPUT" || tag === "TEXTAREA") return;
		e.preventDefault();
		if (_helpOpen()) closeHelp();
		else openHelp();
		return;
	}

	// Flat-sidebar list navigation: ↑/↓ move the cursor, ⏎ opens the row.
	// Bail for: open modals (CommandPalette owns its own ↑/↓), editable
	// targets (focused PTY/inputs keep their arrows — shell history), and
	// any modifier combo.
	if (e.key === "ArrowUp" || e.key === "ArrowDown" || e.key === "Enter") {
		if (_cmdPaletteOpen() || _helpOpen()) return;
		if (isEditableTarget(e.target)) return;
		if (terminalOwnsKeyboard()) return;
		if (e.metaKey || e.ctrlKey || e.altKey || e.shiftKey) return;
		// Belt-and-suspenders: ⏎ activating the sidebar cursor NAVIGATES to another
		// session — the boot-loop failure mode. Even if terminalOwnsKeyboard() ever
		// regresses, ⏎ must never teleport while a terminal deck is mounted. (↑/↓
		// only move a highlight and are harmless, so they don't need this backstop.)
		if (
			e.key === "Enter" &&
			document.querySelector('[data-testid="terminal-deck"]')
		)
			return;
		e.preventDefault();
		if (e.key === "Enter") {
			activateCursor();
			return;
		}
		moveCursor(e.key === "ArrowDown" ? 1 : -1);
		return;
	}
}

let installed = false;

export function installKeyboardShortcuts(): () => void {
	if (installed)
		return () => {
			/* already installed */
		};
	installed = true;
	window.addEventListener("keydown", handleKeydown, { capture: true });
	return () => {
		installed = false;
		window.removeEventListener("keydown", handleKeydown, { capture: true });
	};
}
