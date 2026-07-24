// Markdown rendering for omp chat. Uses `marked` (the standard library pi-web's
// MarkdownBody uses) — NOT a hand-rolled renderer (per plan: "do NOT invent a
// new renderer"). Mermaid/katex are a follow-up; plain markdown ships now.
// Output is sanitized of raw HTML (omp markdown is model output, not trusted).

import { marked } from "marked";

marked.setOptions({ gfm: true, breaks: false, async: false });

/** Render markdown → sanitized HTML string. marked emits HTML; we strip raw
 *  <script>/<style> and event-handler attrs so model output can't inject. */
export function renderMarkdown(md: string): string {
	if (!md) return "";
	let html: string;
	try {
		html = marked.parse(md) as string;
	} catch {
		return escapeHtml(md);
	}
	return sanitizeHtml(html);
}

// Minimal sanitizer: drop script/style blocks + on* handler attrs + javascript: URLs.
// DOMPurify isn't a dep; this covers the model-output threat model (no untrusted
// third-party HTML — omp itself never emits <script>, but defense in depth).
function sanitizeHtml(html: string): string {
	return html
		.replace(/<\s*(script|style|iframe|object|embed)[\s\S]*?<\/\s*\1\s*>/gi, "")
		.replace(/\son\w+\s*=\s*"[^"]*"/gi, "")
		.replace(/\son\w+\s*=\s*'[^']*'/gi, "")
		.replace(/javascript:/gi, "");
}

function escapeHtml(s: string): string {
	return s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");
}
