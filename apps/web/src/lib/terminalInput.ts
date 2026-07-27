/** Turns one printable key into its ASCII Ctrl byte. IME and unsupported data
 * pass through unchanged so the one-shot caller can safely disarm. */
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
