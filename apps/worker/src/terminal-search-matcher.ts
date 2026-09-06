// Linear-time row matching for terminal content search.
// The scrollback scanner compiles one expression per request and calls these
// visitors for each cell-derived row; tests reach the underscored regex seam.

import { RE2JS } from "re2js";

export type TerminalSearchExpression = RE2JS;
export type RowMatchVisitor = (offset: number, length: number) => void;

export function compileTerminalSearchExpression(
	query: string,
	regex: boolean,
	caseSensitive: boolean,
): TerminalSearchExpression | null {
	if (!regex && caseSensitive) return null;
	return RE2JS.compile(
		regex ? query : RE2JS.quote(query),
		caseSensitive ? 0 : RE2JS.CASE_INSENSITIVE,
	);
}

export function visitPlainRowMatches(
	text: string,
	needle: string,
	visit: RowMatchVisitor,
): void {
	if (needle.length === 0) return;
	let offset = text.indexOf(needle);
	while (offset >= 0) {
		visit(offset, needle.length);
		offset = text.indexOf(needle, offset + needle.length);
	}
}

/** Match offsets are UTF-16 units, matching spansText and textRangeToColumns. */
export function _visitRegexRowMatches(
	text: string,
	expression: TerminalSearchExpression,
	visit: RowMatchVisitor,
): void {
	for (const hit of expression.matchAll(text)) {
		visit(hit.index ?? 0, hit[0]!.length);
	}
}
