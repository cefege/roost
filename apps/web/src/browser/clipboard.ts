// Clipboard write helper for every user-initiated copy in the web app.
//
// navigator.clipboard.writeText rejects for reasons the UI can't fix at call
// time — permission denial, insecure context, no activation — and each call
// site previously re-implemented its own try/catch, drifting between silent
// swallows, confirm-state resets and status messages. Centralizing here makes
// the failure mode uniform: resolve `false`, never throw, so callers branch on
// a boolean instead of guarding an exception.

/** Write `text` to the system clipboard. Resolves false when the write is
 *  denied (permission, insecure context) instead of throwing — the caller
 *  decides what the user sees, this only reports success. */
export async function copyToClipboard(text: string): Promise<boolean> {
	try {
		await navigator.clipboard.writeText(text);
		return true;
	} catch {
		return false;
	}
}
