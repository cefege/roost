import { describe, expect, test } from "bun:test";
import { buildPtyPayload } from "../src/lib/ptyPaste.ts";
import {
	applyCtrlModifier,
	isAltGraphKey,
	terminalKeySequence,
} from "../src/lib/terminalInput.ts";


const decode = (bytes: Uint8Array): string => new TextDecoder().decode(bytes);

describe("buildPtyPayload", () => {
	test("emits raw UTF-8 when bracketed paste mode is disabled", () => {
		expect(decode(buildPtyPayload("echo hi", false))).toBe("echo hi");
	});

	test("frames every paste in bracketed paste mode", () => {
		expect(decode(buildPtyPayload("echo hi", true))).toBe(
			"\x1b[200~echo hi\x1b[201~",
		);
		expect(decode(buildPtyPayload("one\ntwo", true))).toBe(
			"\x1b[200~one\rtwo\x1b[201~",
		);
	});

	test("normalizes CRLF, LF and CR to exactly one terminal CR", () => {
		expect(decode(buildPtyPayload("a\r\nb\nc\rd", false))).toBe("a\rb\rc\rd");
		expect(decode(buildPtyPayload("\r\n", true))).toBe(
			"\x1b[200~\r\x1b[201~",
		);
	});

	test("removes embedded escape bytes from a bracketed paste", () => {
		expect(decode(buildPtyPayload("safe\x1b[201~text", true))).toBe(
			"\x1b[200~safe[201~text\x1b[201~",
		);
	});
});

describe("applyCtrlModifier", () => {
	test("maps terminal Ctrl characters and leaves unsupported input intact", () => {
		for (const char of "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz") {
			expect(applyCtrlModifier(char)).toBe(
				String.fromCharCode(char.charCodeAt(0) & 0x1f),
			);
		}
		const controls = [
			[" ", "\0"],
			["@", "\0"],
			["[", "\x1b"],
			["\\", "\x1c"],
			["]", "\x1d"],
			["^", "\x1e"],
			["_", "\x1f"],
			["?", "\x7f"],
		] as const;
		for (const [input, expected] of controls) {
			expect(applyCtrlModifier(input)).toBe(expected);
		}
		expect(applyCtrlModifier("1")).toBe("1");
		expect(applyCtrlModifier("文字")).toBe("文字");
	});
});

describe("terminalKeySequence", () => {
	test("switches cursor and home/end keys with DECCKM application mode", () => {
		expect(terminalKeySequence({ key: "ArrowUp" }, false)).toBe("\x1b[A");
		expect(terminalKeySequence({ key: "ArrowUp" }, true)).toBe("\x1bOA");
		expect(terminalKeySequence({ key: "Home" }, false)).toBe("\x1b[H");
		expect(terminalKeySequence({ key: "End" }, true)).toBe("\x1bOF");
	});

	test("encodes navigation, function and special-key modifiers", () => {
		expect(terminalKeySequence({ key: "ArrowUp", shiftKey: true }, false)).toBe("\x1b[1;2A");
		expect(terminalKeySequence({ key: "ArrowRight", ctrlKey: true }, false)).toBe("\x1b[1;5C");
		expect(terminalKeySequence({ key: "F1", ctrlKey: true, shiftKey: true }, false)).toBe("\x1b[1;6P");
		expect(terminalKeySequence({ key: "F12", altKey: true }, false)).toBe("\x1b[24;3~");
		expect(terminalKeySequence({ key: "Tab", shiftKey: true }, false)).toBe("\x1b[Z");
		expect(terminalKeySequence({ key: "Enter", shiftKey: true }, false)).toBe("\x1b[13;2u");
		expect(terminalKeySequence({ key: "Enter" }, false)).toBe("\r");
	});

	test("preserves text, Ctrl, Alt and Meta ownership", () => {
		expect(terminalKeySequence({ key: "é" }, false)).toBe("é");
		expect(terminalKeySequence({ key: "x", altKey: true }, false)).toBe("\x1bx");
		expect(terminalKeySequence({ key: "c", ctrlKey: true }, false)).toBe("\x03");
		expect(terminalKeySequence({ key: "x", metaKey: true }, false)).toBeNull();
		expect(terminalKeySequence({ key: "Dead" }, false)).toBeNull();
		expect(terminalKeySequence({ key: "Process", isComposing: true }, false)).toBeNull();
	});

	test("treats explicit and Ctrl+Alt-reported AltGraph as printable text", () => {
		const explicit = { key: "€", ctrlKey: true, altKey: true, altGraph: true };
		const represented = { key: "@", ctrlKey: true, altKey: true };
		expect(isAltGraphKey(explicit)).toBe(true);
		expect(isAltGraphKey(represented)).toBe(true);
		expect(terminalKeySequence(explicit, false)).toBe("€");
		expect(terminalKeySequence(represented, false)).toBe("@");
	});
});
