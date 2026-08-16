/** Minimal keyboard event surface consumed by the terminal encoder. Keeping the
 * encoder DOM-free makes application-mode and platform behavior deterministic
 * in tests and lets touch controls use the same path as a physical keyboard. */
export interface TerminalKeyEvent {
	key: string;
	code?: string;
	shiftKey?: boolean;
	altKey?: boolean;
	ctrlKey?: boolean;
	metaKey?: boolean;
	isComposing?: boolean;
	/** Test/synthetic-event seam. Real events use getModifierState("AltGraph"). */
	altGraph?: boolean;
	getModifierState?: (key: string) => boolean;
}

const NORMAL_NAV: Readonly<Record<string, string>> = {
	ArrowUp: "\x1b[A",
	ArrowDown: "\x1b[B",
	ArrowRight: "\x1b[C",
	ArrowLeft: "\x1b[D",
	Home: "\x1b[H",
	End: "\x1b[F",
};

const APPLICATION_NAV: Readonly<Record<string, string>> = {
	ArrowUp: "\x1bOA",
	ArrowDown: "\x1bOB",
	ArrowRight: "\x1bOC",
	ArrowLeft: "\x1bOD",
	Home: "\x1bOH",
	End: "\x1bOF",
};

const NAV_FINAL: Readonly<Record<string, string>> = {
	ArrowUp: "A",
	ArrowDown: "B",
	ArrowRight: "C",
	ArrowLeft: "D",
	Home: "H",
	End: "F",
};

const TILDE_KEYS: Readonly<Record<string, number>> = {
	Insert: 2,
	Delete: 3,
	PageUp: 5,
	PageDown: 6,
	F5: 15,
	F6: 17,
	F7: 18,
	F8: 19,
	F9: 20,
	F10: 21,
	F11: 23,
	F12: 24,
};

const SS3_FUNCTION_KEYS: Readonly<Record<string, string>> = {
	F1: "P",
	F2: "Q",
	F3: "R",
	F4: "S",
};

const SIMPLE_KEYS: Readonly<Record<string, string>> = {
	Enter: "\r",
	Backspace: "\x7f",
	Tab: "\t",
	Escape: "\x1b",
};

/** True for one printable Unicode code point (including astral characters). */
export function isTerminalPrintableKey(key: string): boolean {
	return key !== "Dead" && key !== "Process" && key !== "Unidentified"
		&& Array.from(key).length === 1;
}

/** Windows reports AltGraph either explicitly or as Ctrl+Alt plus the resulting
 * printable character. Treat both forms as text input, never as a Ctrl binding
 * with an Alt escape prefix. */
export function isAltGraphKey(event: TerminalKeyEvent): boolean {
	if (event.altGraph === true) return true;
	try {
		if (event.getModifierState?.("AltGraph") === true) return true;
	} catch {
		// Synthetic events may expose a throwing getModifierState implementation.
	}
	return event.ctrlKey === true
		&& event.altKey === true
		&& event.metaKey !== true
		&& isTerminalPrintableKey(event.key);
}

/** Turns one printable key into its ASCII Ctrl byte. IME and unsupported data
 * pass through unchanged so the one-shot touch Ctrl caller can safely disarm. */
export function applyCtrlModifier(data: string): string {
	if (data.length !== 1) return data;
	const code = data.charCodeAt(0);
	if ((code >= 65 && code <= 90) || (code >= 97 && code <= 122)) {
		return String.fromCharCode(code & 0x1f);
	}
	if (data === " " || data === "@") return "\0";
	if (data === "[") return "\x1b";
	if (data === "\\") return "\x1c";
	if (data === "]") return "\x1d";
	if (data === "^") return "\x1e";
	if (data === "_") return "\x1f";
	if (data === "?") return "\x7f";
	return data;
}

function modifierParameter(event: TerminalKeyEvent): number {
	return 1
		+ (event.shiftKey ? 1 : 0)
		+ (event.altKey ? 2 : 0)
		+ (event.ctrlKey ? 4 : 0);
}

/** Encode one terminal key using the worker-reported DECCKM cursor mode.
 * Printable/dead-key text produced by IME also arrives through the textarea's
 * input event; callers must leave composing/dead keydowns unhandled so that
 * browser text services can finish them exactly once. */
export function terminalKeySequence(
	event: TerminalKeyEvent,
	cursorKeysApplication: boolean,
): string | null {
	if (event.isComposing || event.key === "Dead" || event.key === "Process"
		|| event.key === "Unidentified") return null;

	const altGraph = isAltGraphKey(event);
	if (altGraph && isTerminalPrintableKey(event.key)) return event.key;

	// A real Meta shortcut belongs to the browser/app. The controller handles
	// the two terminal-specific macOS exceptions (Meta+Backspace and Meta+A).
	if (event.metaKey && !event.ctrlKey) return null;

	if (event.ctrlKey && !event.altKey && !event.metaKey
		&& isTerminalPrintableKey(event.key)) {
		const controlled = applyCtrlModifier(event.key);
		return controlled === event.key ? null : controlled;
	}

	if (event.key === "Enter" && event.shiftKey) {
		return `\x1b[13;${modifierParameter(event)}u`;
	}
	if (event.key === "Tab" && event.shiftKey && !event.altKey && !event.ctrlKey) {
		return "\x1b[Z";
	}

	const nav = NAV_FINAL[event.key];
	if (nav) {
		const modifier = modifierParameter(event);
		if (modifier === 1) {
			return (cursorKeysApplication ? APPLICATION_NAV : NORMAL_NAV)[event.key] ?? null;
		}
		return `\x1b[1;${modifier}${nav}`;
	}

	const tilde = TILDE_KEYS[event.key];
	if (tilde !== undefined) {
		const modifier = modifierParameter(event);
		return modifier === 1 ? `\x1b[${tilde}~` : `\x1b[${tilde};${modifier}~`;
	}

	const functionFinal = SS3_FUNCTION_KEYS[event.key];
	if (functionFinal) {
		const modifier = modifierParameter(event);
		return modifier === 1
			? `\x1bO${functionFinal}`
			: `\x1b[1;${modifier}${functionFinal}`;
	}

	const simple = SIMPLE_KEYS[event.key];
	if (simple) return event.altKey ? `\x1b${simple}` : simple;

	if (isTerminalPrintableKey(event.key) && !event.ctrlKey && !event.metaKey) {
		return event.altKey ? `\x1b${event.key}` : event.key;
	}
	return null;
}
