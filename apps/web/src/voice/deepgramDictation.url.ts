// Deepgram handshake-URL builder + its bounding constants. Split out of
// deepgramDictation.ts (400-line cap). buildUrl is re-exported from
// deepgramDictation.ts so the connect-reliability test
// (apps/web/tests/buildUrl.test.ts) imports it from the same path.

// CONCLUSIVE FIX (plan/voice-keyterm-conclusive-fix.md). The recurring "voice
// dies depending on what's on screen" bug is STRUCTURAL: keyterms ride in the
// handshake URL, the URL is built from unbounded/adversarial screen content, and
// Deepgram enforces caps AT HANDSHAKE → a long/dense screen = 1006, recording
// never starts. Deepgram (docs verified 2026-07-09) offers NO post-connect
// keyterm channel for nova-3 (Flux-only), so the URL is the only transport.
// Therefore we BOUND the keyterm contribution to the URL by construction so it
// can never grow enough to break the handshake — screen content stops being
// able to prevent recording. Deepgram itself recommends "the most important
// 20-50 terms"; 50 aligns the bound to their guidance.
const MAX_KEYTERM_COUNT = 50; // Deepgram's own "20-50 terms" recommendation
const MAX_KEYTERM_URL_BUDGET = 1500; // bytes of keyterm= params — tiny vs the ceiling
// Final ASSERTION only — unreachable now that the two bounds above cap the
// keyterm contribution to a small, screen-independent window.
const MAX_WS_URL_LEN = 8000;

// Exported for the connect-reliability regression test (buildUrl.test.ts):
// the invariant "screen content can never make the URL unconnectable" is
// asserted directly against this pure function.
export function buildUrl(lang: string, keyterms: string[]): string {
	const base = new URLSearchParams({
		model: "nova-3",
		encoding: "linear16",
		sample_rate: "16000",
		channels: "1",
		smart_format: "true",
		punctuate: "true",
		interim_results: "true",
	});
	const isEnglish =
		lang !== "__auto__" && lang !== "multi" && (lang || "en").startsWith("en");
	if (lang === "__auto__") base.set("detect_language", "true");
	else if (lang === "multi") base.set("language", "multi");
	else base.set("language", lang || "en");
	// Contextual biasing toward on-screen project jargon (keytermContext).
	// Deepgram keyterm prompting is English-only — sending it with multi/auto
	// makes Deepgram reject the socket, so gate it. Append in RANK order (highest
	// score first), stopping at whichever bound binds first (count OR bytes) so
	// the URL length is bounded by CONSTRUCTION, independent of screen content.
	const terms: string[] = [];
	if (isEnglish) {
		let keytermBytes = 0;
		for (const t of keyterms) {
			if (terms.length >= MAX_KEYTERM_COUNT) break;
			const cost = 9 + encodeURIComponent(t).length; // "&keyterm=" + encoded value
			if (keytermBytes + cost > MAX_KEYTERM_URL_BUDGET) break;
			terms.push(t);
			keytermBytes += cost;
		}
	}
	const build = (ts: string[]): string => {
		const p = new URLSearchParams(base);
		for (const t of ts) p.append("keyterm", t);
		return "wss://api.deepgram.com/v1/listen?" + p.toString();
	};
	let url = build(terms);
	while (url.length > MAX_WS_URL_LEN && terms.length > 0) {
		terms.pop(); // unreachable final assertion — the bounds above already cap it
		url = build(terms);
	}
	return url;
}
