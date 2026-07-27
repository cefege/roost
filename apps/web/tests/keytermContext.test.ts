// keytermContext — TF-IDF-lite keyterm extraction from terminal text.
// Asserts: code-shaped jargon survives, common words drop, spoken forms emit,
// grid outranks deep scrollback, 500-token budget holds.

import { describe, test, expect } from "bun:test";
import {
	extractKeyterms,
	spokenForm,
	isSpeakable,
} from "../src/lib/keytermContext.ts";

describe("spokenForm", () => {
	test("splits camelCase / snake / kebab / path", () => {
		expect(spokenForm("deepgramDictation")).toBe("deepgram dictation");
		expect(spokenForm("sample_rate")).toBe("sample rate");
		expect(spokenForm("nova-3")).toBe("nova 3");
		expect(spokenForm("coordFactory")).toBe("coord factory");
		expect(spokenForm("RPCClient")).toBe("rpc client");
	});
	test("returns null for single-word tokens (nothing to split)", () => {
		expect(spokenForm("Kysely")).toBeNull();
		expect(spokenForm("tailnet")).toBeNull();
	});
});

describe("extractKeyterms", () => {
	const ctx = (grid: string, scrollback = "", input?: string) => ({
		grid,
		scrollback,
		input,
	});

	test("keeps code-shaped jargon, drops common English", () => {
		// Design: false-positive keyterms are ~free, false-negatives are the bug —
		// so the load-bearing assertions are "jargon kept" + "articles/preps drop".
		const out = extractKeyterms(
			ctx("the worker dialed coordFactory over tailnet using Kysely"),
		);
		const low = out.map((t) => t.toLowerCase());
		expect(low).toContain("coordfactory");
		expect(low).toContain("tailnet");
		expect(low).toContain("kysely");
		expect(low).not.toContain("the");
		expect(low).not.toContain("over");
	});

	test("emits raw AND spoken form for identifiers", () => {
		const out = extractKeyterms(ctx("open deepgramDictation now"));
		expect(out).toContain("deepgramDictation");
		expect(out).toContain("deepgram dictation");
	});

	test("path token reduces to basename, strips extension", () => {
		const out = extractKeyterms(
			ctx("edit apps/web/src/lib/cellRenderer.ts please"),
		);
		expect(out).toContain("cellRenderer");
		expect(out.join(" ")).not.toMatch(/apps|\.ts/);
	});

	test("grid outranks deep scrollback for same-frequency term", () => {
		// gridTerm appears once on the visible grid; oldTerm once at scrollback top.
		const out = extractKeyterms(
			ctx("gridTermXyz", "oldTermXyz\n" + "filler line\n".repeat(40)),
		);
		expect(out.indexOf("gridTermXyz")).toBeLessThan(out.indexOf("oldTermXyz"));
	});

	test("user input is highest-signal — outranks a grid-only term", () => {
		const out = extractKeyterms(ctx("gridOnlyTok", "", "typedTok"));
		expect(out.indexOf("typedTok")).toBeLessThan(out.indexOf("gridOnlyTok"));
	});

	test("respects the 500-token Deepgram budget", () => {
		const many = Array.from({ length: 400 }, (_, i) => `tokenName${i}`).join(
			" ",
		);
		const out = extractKeyterms(ctx(many));
		const tokens = out.reduce((n, t) => n + t.trim().split(/\s+/).length, 0);
		expect(tokens).toBeLessThanOrEqual(500);
	});

	// Regression: 128+ keyterm params reset Deepgram's WS handshake (close 1006).
	// Deepgram caps entries at ~100; each ranked term emits raw + spoken, so the
	// emitted array must stay capped regardless of how much jargon is on screen.
	test("caps emitted keyterm entries under Deepgram's ~100 limit", () => {
		const many = Array.from(
			{ length: 400 },
			(_, i) => `camelCaseTerm${i}`,
		).join(" ");
		const out = extractKeyterms(ctx(many));
		expect(out.length).toBeLessThanOrEqual(80);
	});

	test("mines Capitalized bigrams as multi-word keyterms", () => {
		const out = extractKeyterms(
			ctx("opened the Cell Terminal and the Coord Link panel"),
		);
		expect(out).toContain("Cell Terminal");
		expect(out).toContain("Coord Link");
	});

	test("input phrase outranks a grid-only unigram", () => {
		const out = extractKeyterms(ctx("randomGridTok", "", "Session Manager"));
		expect(out).toContain("Session Manager");
		expect(out.indexOf("Session Manager")).toBeLessThan(
			out.indexOf("randomGridTok"),
		);
	});

	test("drops pure numbers and hash-length blobs", () => {
		const out = extractKeyterms(
			ctx("commit a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2 at 12345"),
		);
		expect(out).not.toContain("12345");
		expect(out.every((t) => t.length <= 40)).toBe(true);
	});

	test("drops commit SHAs but keeps all-hex English words", () => {
		const out = extractKeyterms(
			ctx("rebased onto 9c517120 the facade module"),
		).map((t) => t.toLowerCase());
		expect(out).not.toContain("9c517120");
		expect(out).toContain("facade"); // all hex-letters, no digit → kept
	});

	// KTF3-JUNK-TOKENS regression: a pi harness paints box-drawing headers,
	// spinner frames, ANSI residue, and CJK paths ordinary prompts never did. Those
	// survived `keep` and reached buildUrl → 1006 handshake reset (recording
	// never starts). Every emitted keyterm MUST be printable-ASCII & speakable.
	test("drops handshake-hostile pi-grid junk; every emitted term is speakable", () => {
		const piGrid = [
			"╭─────────── pi ───────────╮",
			"│ ⠋ thinking…  coordFactory │",
			"╰──────────────────────────╯",
			"[2m38;5;244mstatus[0m tailnet ready",
			"读取文件 apps/web/src/lib/deepgramDictation.ts",
			"user@host:~/Code/idea/apps/web$ Kysely migrate",
		].join("\n");
		const out = extractKeyterms(ctx(piGrid));
		for (const t of out) expect(t).toMatch(/^[\x20-\x7E]+$/); // no control/box-draw/CJK
		for (const t of out) expect(isSpeakable(t)).toBe(true);
		// The real jargon still survives the sanitizer.
		const low = out.map((t) => t.toLowerCase());
		expect(low).toContain("coordfactory");
		expect(low).toContain("kysely");
		expect(low).toContain("tailnet");
	});

	// KTF1-BYTE-BUDGET regression: the binding constraint is on-wire URL bytes,
	// not the whitespace/BPE proxy. A busy screen must not push the keyterm= param
	// bytes past the budget (→ Deepgram 1006 with no Error frame).
	test("holds the on-wire keyterm byte budget on a dense screen", () => {
		const many = Array.from(
			{ length: 400 },
			(_, i) => `longCamelCaseIdentifier${i}`,
		).join(" ");
		const out = extractKeyterms(ctx(many));
		const urlBytes = out.reduce(
			(n, t) => n + 8 + encodeURIComponent(t).length,
			0,
		);
		expect(urlBytes).toBeLessThanOrEqual(4000);
		expect(out.length).toBeGreaterThan(0); // budget trims, doesn't empty
	});
});

describe("isSpeakable", () => {
	test("rejects control / box-drawing / CJK / emoji, accepts clean words", () => {
		expect(isSpeakable("coordFactory")).toBe(true);
		expect(isSpeakable("coord factory")).toBe(true);
		expect(isSpeakable("nova-3")).toBe(true);
		expect(isSpeakable("╭────╮")).toBe(false);
		expect(isSpeakable("读取文件")).toBe(false);
		expect(isSpeakable("\x1b[2m")).toBe(false);
		expect(isSpeakable("apps/web/src/lib/x")).toBe(false); // @//-heavy path residue
		expect(isSpeakable("12")).toBe(false); // no real word content
	});
});
