// Pane-local terminal input controller. It owns one cheap textarea per mounted
// pane and encodes keys directly from worker-reported terminal modes; there is no
// browser-side VT core, WASM instance, mirrored grid, or shared focus singleton.

import { diag } from "@roost/shared/diag";
import {
	terminalKeySequence,
	type TerminalKeyEvent,
} from "./terminalInput.ts";

export interface TerminalInputControllerOptions {
	cursorKeysApplication: () => boolean;
	/** DECSET 1004 as the worker reports it. When the application asked for focus
	 *  reporting, real textarea focus/blur is a PTY event, not just styling. */
	focusEventsEnabled: () => boolean;
	onData: (data: string) => void;
	/** Clipboard admission belongs to the pane so multiline confirmation,
	 * attachment extraction, bracketed framing, and queue limits share one path. */
	onPaste: (text: string, event: ClipboardEvent) => void;
	ariaLabel?: string;
}

export class TerminalInputController {
	readonly textarea: HTMLTextAreaElement;

	private composing = false;
	private compositionToken = 0;
	private destroyed = false;
	private readonly doc: Document;
	/** Focus as the APPLICATION last saw it. CSI I / CSI O report a transition, so
	 *  a repeated focus event (forceFocus dispatches one explicitly) reports once. */
	private reportedFocus = false;
	/** Set across forceFocus's blur-then-focus dance: that blur is an
	 *  implementation detail of re-firing focus, not the pane losing the keyboard,
	 *  and reporting it would tell the application to run its FocusLost handling
	 *  every time the focus effect re-ran. */
	private refocusing = false;

	private readonly onKeyDown = (event: KeyboardEvent): void => {
		if (this.composing || event.isComposing || event.key === "Dead"
			|| event.key === "Process" || event.key === "Unidentified") return;

		const lower = event.key.toLowerCase();
		if ((event.metaKey || event.ctrlKey) && lower === "c") {
			const selection = this.doc.getSelection();
			if (selection && selection.toString().length > 0) return;
		}
		if ((event.metaKey || event.ctrlKey) && lower === "v") {
			// Do not prevent the platform paste. Its ClipboardEvent is admitted once
			// through onPaste below, including files and bracketed-paste framing.
			this.textarea.focus({ preventScroll: true });
			return;
		}
		if (event.metaKey && !event.ctrlKey) {
			if (event.key === "Backspace") {
				event.preventDefault();
				this.options.onData("\x15");
			} else if (lower === "a") {
				event.preventDefault();
				const selection = this.doc.getSelection();
				if (selection) {
					const range = this.doc.createRange();
					range.selectNodeContents(this.root);
					selection.removeAllRanges();
					selection.addRange(range);
				}
			}
			return;
		}

		// Match terminal ownership: once the focused textarea receives a non-IME,
		// non-platform key, the browser must not scroll or activate surrounding UI.
		event.preventDefault();
		const sequence = terminalKeySequence(event, this.options.cursorKeysApplication());
		if (sequence !== null) this.options.onData(sequence);
	};

	private readonly onPaste = (event: ClipboardEvent): void => {
		event.preventDefault();
		this.textarea.value = "";
		this.options.onPaste(event.clipboardData?.getData("text") ?? "", event);
	};

	private readonly onCompositionStart = (): void => {
		this.composing = true;
		this.compositionToken += 1;
		this.textarea.value = "";
	};

	private readonly onCompositionEnd = (event: CompositionEvent): void => {
		this.composing = false;
		const token = ++this.compositionToken;
		const committed = event.data || this.textarea.value;
		// Chromium dispatches the final input in the same native event turn; Safari
		// variants may not dispatch it at all. Defer a fallback one microtask so the
		// input handler can cancel it, guaranteeing one commit in either ordering.
		queueMicrotask(() => {
			if (this.destroyed || token !== this.compositionToken) return;
			this.compositionToken += 1;
			const value = this.textarea.value || committed;
			this.textarea.value = "";
			if (value) this.options.onData(value);
		});
	};

	private readonly onInput = (event: InputEvent): void => {
		if (this.composing || event.isComposing) return;
		// Cancels compositionend's fallback when this is the final committed input.
		this.compositionToken += 1;
		const value = this.textarea.value || event.data || "";
		this.textarea.value = "";
		if (value) this.options.onData(value);
	};

	// Focus reporting rides the REAL textarea transition, not the pane-level focus
	// orchestration: the application asked which surface owns the keyboard now,
	// and only these two events answer that.
	private readonly reportFocus = (focused: boolean): void => {
		if (this.refocusing || this.reportedFocus === focused) return;
		this.reportedFocus = focused;
		if (this.options.focusEventsEnabled()) this.options.onData(focused ? "\x1b[I" : "\x1b[O");
	};

	private readonly onFocus = (): void => {
		this.root.classList.add("focused");
		this.reportFocus(true);
	};

	private readonly onBlur = (): void => {
		this.root.classList.remove("focused");
		this.reportFocus(false);
	};

	constructor(
		private readonly root: HTMLElement,
		private readonly options: TerminalInputControllerOptions,
	) {
		this.doc = root.ownerDocument;
		const textarea = this.doc.createElement("textarea");
		this.textarea = textarea;
		textarea.className = "terminal-input";
		textarea.setAttribute("autocapitalize", "off");
		textarea.setAttribute("autocomplete", "off");
		textarea.setAttribute("autocorrect", "off");
		textarea.setAttribute("spellcheck", "false");
		textarea.setAttribute("enterkeyhint", "send");
		textarea.setAttribute("aria-label", options.ariaLabel ?? "Terminal input");
		textarea.tabIndex = 0;
		textarea.addEventListener("keydown", this.onKeyDown);
		textarea.addEventListener("paste", this.onPaste);
		textarea.addEventListener("compositionstart", this.onCompositionStart);
		textarea.addEventListener("compositionend", this.onCompositionEnd);
		textarea.addEventListener("input", this.onInput);
		textarea.addEventListener("focus", this.onFocus);
		textarea.addEventListener("blur", this.onBlur);
		root.appendChild(textarea);
	}

	/** Blur-first focus dance retained from the old input host. Focusing an
	 * already-active textarea does not emit focus, so blur guarantees a fresh
	 * native event and the explicit FocusEvent keeps pane styling deterministic. */
	forceFocus(): void {
		if (this.destroyed) return;
		try {
			if (this.doc.activeElement === this.textarea) {
				this.refocusing = true;
				this.textarea.blur();
				this.refocusing = false;
			}
			this.textarea.focus({ preventScroll: true });
			if (this.doc.activeElement === this.textarea) {
				const FocusEventCtor = this.doc.defaultView?.FocusEvent
					?? globalThis.FocusEvent;
				if (typeof FocusEventCtor === "function") {
					this.textarea.dispatchEvent(new FocusEventCtor("focus", { bubbles: true }));
				}
			}
		} catch {
			// A detached pane can race cleanup: never leave the dance's report guard
			// latched, or focus reporting dies silently for this pane's lifetime.
			this.refocusing = false;
		}
		diag("focus.force", {
			landed: this.doc.activeElement === this.textarea,
			has_textarea: this.textarea.isConnected,
		});
	}

	/** Touch navigation and focus recovery use exactly the physical-key encoder
	 * without synthesizing a second DOM event (which can duplicate IME input). */
	dispatchKeydown(key: string, init: Omit<TerminalKeyEvent, "key"> = {}): boolean {
		if (this.destroyed) return false;
		const sequence = terminalKeySequence(
			{ ...init, key },
			this.options.cursorKeysApplication(),
		);
		if (sequence === null) return false;
		this.options.onData(sequence);
		return true;
	}

	ownsTarget(target: Element | null): boolean {
		return target === this.textarea;
	}

	setAccessibleLabel(label: string): void {
		this.textarea.setAttribute("aria-label", label);
	}

	destroy(): void {
		if (this.destroyed) return;
		this.destroyed = true;
		this.compositionToken += 1;
		this.textarea.removeEventListener("keydown", this.onKeyDown);
		this.textarea.removeEventListener("paste", this.onPaste);
		this.textarea.removeEventListener("compositionstart", this.onCompositionStart);
		this.textarea.removeEventListener("compositionend", this.onCompositionEnd);
		this.textarea.removeEventListener("input", this.onInput);
		this.textarea.removeEventListener("focus", this.onFocus);
		this.textarea.removeEventListener("blur", this.onBlur);
		this.root.classList.remove("focused");
		this.textarea.remove();
	}
}
