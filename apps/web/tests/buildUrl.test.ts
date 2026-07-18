// buildUrl — CONCLUSIVE-FIX invariant lock (plan/voice-keyterm-conclusive-fix.md).
// The recurring "voice recording dies depending on what's on screen" bug is
// structural: keyterms ride in the handshake URL, built from unbounded screen
// content. This asserts the cure: screen content can NEVER make the URL
// unconnectable — the keyterm contribution is bounded by CONSTRUCTION (count +
// bytes), independent of how pathological the input is.

import { describe, test, expect } from "bun:test";
import { buildUrl } from "../src/lib/deepgramDictation.ts";
import { extractKeyterms } from "../src/lib/keytermContext.ts";

const MAX_WS_URL_LEN = 8000;
const MAX_KEYTERM_COUNT = 50;

describe("buildUrl connect-reliability invariant", () => {
	test("a pathological 10k-term screen still yields a connectable URL", () => {
		// Simulate the worst case: a busy pi screen exploded into thousands of
		// long identifiers. The URL MUST stay under the handshake ceiling.
		const terms = Array.from(
			{ length: 10_000 },
			(_, i) => `veryLongCamelCaseIdentifierNumber${i}`,
		);
		const url = buildUrl("en", terms);
		expect(url.length).toBeLessThan(MAX_WS_URL_LEN);
		// Bounded by construction, not by the trim assertion.
		const keytermCount = (url.match(/[?&]keyterm=/g) ?? []).length;
		expect(keytermCount).toBeLessThanOrEqual(MAX_KEYTERM_COUNT);
		expect(keytermCount).toBeGreaterThan(0); // biasing still present
	});

	test("URL length is screen-INDEPENDENT — 50 terms vs 10k terms bound the same", () => {
		const few = buildUrl(
			"en",
			Array.from({ length: 50 }, (_, i) => `termName${i}`),
		);
		const many = buildUrl(
			"en",
			Array.from({ length: 10_000 }, (_, i) => `termName${i}`),
		);
		// Adding 9,950 more on-screen terms does not grow the URL past the bound.
		expect(few.length).toBeLessThan(MAX_WS_URL_LEN);
		expect(many.length).toBeLessThan(MAX_WS_URL_LEN);
	});

	test("real extractKeyterms output over a hostile pi screen stays connectable", () => {
		// End-to-end: the actual extractor feeding the actual URL builder.
		const piScreen = [
			"╭─────────── pi ───────────╮",
			"│ ⠋ thinking…  coordFactory │",
			"╰──────────────────────────╯",
			Array.from({ length: 500 }, (_, i) => `jargonToken${i}`).join(" "),
			"读取文件 apps/web/src/lib/deepgramDictation.ts",
		].join("\n");
		const keyterms = extractKeyterms({ grid: piScreen, scrollback: "" });
		const url = buildUrl("en", keyterms);
		expect(url.length).toBeLessThan(MAX_WS_URL_LEN);
	});

	test("non-English langs drop keyterms entirely (Deepgram rejects them)", () => {
		const terms = Array.from({ length: 100 }, (_, i) => `termName${i}`);
		expect(buildUrl("multi", terms)).not.toContain("keyterm=");
		expect(buildUrl("__auto__", terms)).not.toContain("keyterm=");
		expect(buildUrl("en", terms)).toContain("keyterm=");
	});
});
