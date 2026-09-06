// Terminal-search regex engine regression coverage.
// The pathological nonmatch proves row matching cannot monopolize the worker
// through JavaScript RegExp backtracking before deadline checks resume.

import { expect, test } from "bun:test";
import { RE2JS } from "re2js";
import { _visitRegexRowMatches } from "../src/terminal-search-matcher.ts";

test("pathological nested quantifiers complete within one worker slice", () => {
	const expression = RE2JS.compile("(a+)+$");
	const row = `${"a".repeat(255)}!`;
	const matches: Array<{ offset: number; length: number }> = [];
	const startedAt = performance.now();

	_visitRegexRowMatches(row, expression, (offset, length) => {
		matches.push({ offset, length });
	});

	const elapsedMs = performance.now() - startedAt;
	expect(matches).toEqual([]);
	expect(elapsedMs).toBeLessThan(100);
});
