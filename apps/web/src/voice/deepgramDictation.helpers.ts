// Pure helpers for deepgramDictation — keyterm hit-rate diagnostic, WS-close
// classification/messaging, mic-open failure classification, and client-fact
// diagnostics. Split out of deepgramDictation.ts (400-line cap); imported
// back by createDeepgramDictation. No external re-export needed
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

// Baked at build time by vite (define in vite.config.ts), same read as
// VersionBanner. The LITERAL member expression is what vite substitutes —
// reaching it through a variable or optional chain silently yields the
// fallback, which is worse than useless in a build-provenance field.
const BUILD_SHA = (import.meta.env as { VITE_BUILD_SHA?: string }).VITE_BUILD_SHA ?? "dev";

// Enough about the client to tell a phone from a desktop, and a stale cached
// bundle from the running one — the two questions a remote "the mic does
// nothing" report cannot be answered without.
export function clientFacts() {
	return {
		build: BUILD_SHA,
		standalone: typeof matchMedia === "function" && matchMedia("(display-mode: standalone)").matches,
		ua: typeof navigator !== "undefined" ? navigator.userAgent.slice(0, 72) : "",
	};
}

// Classify a getUserMedia/startCapture rejection into the voice.mic_failed
// diag fields plus the user-facing toast line. Each branch names WHY so the
// fix is actionable from the message alone.
export function micOpenFailure(e: unknown): { name: string; detail: string; message: string } {
	// Thrown values here are DOMException/Error-like; anything else degrades to
	// the same "unavailable" fallback the old inline chain produced.
	const err = typeof e === "object" && e !== null ? e : {};
	const name = "name" in err && typeof err.name === "string" ? err.name : "";
	const msg = "message" in err && typeof err.message === "string" ? err.message : "";
	let message: string;
	if (name === "NotAllowedError" || /denied|permission/i.test(msg)) {
		message =
			"Mic blocked for this site — allow it (address-bar icon → Microphone → Allow), then reload.";
	} else if (
		name === "NotReadableError" ||
		name === "AbortError" ||
		/in use|busy|could not start|failed to allocate/i.test(msg)
	) {
		// Another tab/app holds the mic (WhatsApp/Telegram/Zoom/Meet) — macOS
		// hands the device to one page at a time. This is the #1 intermittent
		// "mic error" cause; name it so it's actionable.
		message =
			"Mic is busy — another tab or app (WhatsApp, Telegram, Zoom…) is using it. Close it, then retry.";
	} else if (name === "NotFoundError") {
		message = "No microphone found on this device.";
	} else {
		message = "Mic: " + (msg || name || "unavailable");
	}
	return { name, detail: msg, message };
}
