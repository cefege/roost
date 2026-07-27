import { describe, expect, test } from "bun:test";
import { buildPtyPayload } from "../src/lib/ptyPaste.ts";
import { applyCtrlModifier } from "../src/lib/terminalInput.ts";


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
			"\x1b[200~one\ntwo\x1b[201~",
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
