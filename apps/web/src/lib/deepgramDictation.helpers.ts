// Pure helpers for deepgramDictation — keyterm hit-rate diagnostic + WS-close
// classification/messaging. Split out of deepgramDictation.ts (400-line cap);
// imported back by createDeepgramDictation. No external re-export needed
// (module-internal, same as before the split).

// How many injected keyterms surfaced in the final transcript — the tuning
// signal for whether biasing is pulling its weight (P4). Case-insensitive
// whole-word/phrase containment.
export function keytermHitRate(keyterms: string[], transcript: string): number {
	if (keyterms.length === 0) return 0;
	const hay = ` ${transcript.toLowerCase()} `;
	const hits = keyterms.filter((t) =>
		hay.includes(` ${t.toLowerCase()} `),
	).length;
	return hits / keyterms.length;
}

// A WS close is EXPECTED (no error, no retry) when it's a clean 1000, or the
// user cancelled/stopped — Deepgram closes after our CloseStream/Finalize.
export function isExpectedClose(
	code: number,
	intent: "send" | "cancel" | null,
): boolean {
	return code === 1000 || intent === "cancel" || intent === "send";
}

// Human-readable reason for an unexpected Deepgram close — so the toast says
// WHY (network vs server vs bad key), not just "connection failed".
export function closeMessage(code: number, reason: string): string {
	const r = reason ? `: ${reason}` : "";
	if (code === 1006)
		return "Deepgram connection dropped (network) — retried and still failed. Check your internet.";
	if (code === 1011 || code === 1012 || code === 1013)
		return "Deepgram had a server hiccup — try again in a moment.";
	if (code >= 4000)
		return `Deepgram rejected the session (${code}${r}) — the API key may be invalid or rate-limited (Settings → Voice).`;
	return `Deepgram closed unexpectedly (${code}${r}).`;
}
