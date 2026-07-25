// The ask-tool correlator: omp's RPC select frame carries bare labels, so the
// pane's whole selection card is reconstructed here from tool_execution_start
// args plus the frame's own title/options. Pure, no child process.

import { expect, test } from "bun:test";
import { askQuestionMatches, buildAskChoices, parseAskSpec, splitSelectTitle } from "../src/chat/omp/ask-spec.ts";

const ASK_ARGS = {
	questions: [
		{
			id: "auth",
			question: "Which auth method?",
			header: "Auth",
			options: [
				{ label: "JWT", description: "Bearer tokens for stateless API clients." },
				{ label: "OAuth2", description: "Delegated authorization." },
			],
			recommended: 0,
		},
		{
			id: "features",
			question: "Which features?",
			multi: true,
			options: [{ label: "Streaming" }, { label: "Search", description: "Full text." }],
		},
	],
};

test("parseAskSpec reads the ask schema, defaulting the optional fields", () => {
	expect(parseAskSpec(ASK_ARGS)).toEqual([
		{
			id: "auth",
			question: "Which auth method?",
			header: "Auth",
			options: [
				{ label: "JWT", description: "Bearer tokens for stateless API clients." },
				{ label: "OAuth2", description: "Delegated authorization." },
			],
			multi: false,
		},
		{
			id: "features",
			question: "Which features?",
			header: "",
			options: [
				{ label: "Streaming", description: "" },
				{ label: "Search", description: "Full text." },
			],
			multi: true,
		},
	]);
});

test("parseAskSpec refuses a partial reconstruction", () => {
	// A malformed entry poisons the WHOLE payload: correlating half a batch puts
	// one question's descriptions on another's card, which is worse than none.
	expect(parseAskSpec({ questions: [{ id: "a" }] })).toEqual([]);
	expect(parseAskSpec({ questions: [ASK_ARGS.questions[0], { id: "b", question: "?" }] })).toEqual([]);
	expect(parseAskSpec({ questions: [{ id: "a", question: "?", options: ["raw"] }] })).toEqual([]);
	expect(parseAskSpec({ questions: [] })).toEqual([]);
	expect(parseAskSpec({ path: "README.md" })).toEqual([]);
	expect(parseAskSpec(undefined)).toEqual([]);
});

test("splitSelectTitle strips omp's own title decorations", () => {
	expect(splitSelectTitle("(2 selected) Pick features (3/4)")).toEqual({
		question: "Pick features", progress: "3/4", index: 2,
	});
	expect(splitSelectTitle("Which auth method?")).toEqual({
		question: "Which auth method?", progress: "", index: 0,
	});
	expect(splitSelectTitle("Which auth method? (1/2)")).toEqual({
		question: "Which auth method?", progress: "1/2", index: 0,
	});
	// A question that legitimately ends in parentheses is not a progress suffix.
	expect(splitSelectTitle("Use Redis (in-memory)")).toEqual({
		question: "Use Redis (in-memory)", progress: "", index: 0,
	});
});

test("buildAskChoices joins the spec's descriptions onto the frame's labels", () => {
	const spec = parseAskSpec(ASK_ARGS);
	expect(buildAskChoices(spec, 0, ["JWT (Recommended)", "OAuth2", "Other (type your own)"], new Set())).toEqual([
		{
			// `value` is the RAW frame string — it is echoed back verbatim and omp
			// matches on it; `label` is what the card shows.
			value: "JWT (Recommended)", label: "JWT",
			description: "Bearer tokens for stateless API clients.",
			recommended: true, checked: false, role: "option",
		},
		{
			value: "OAuth2", label: "OAuth2", description: "Delegated authorization.",
			recommended: false, checked: false, role: "option",
		},
		{
			value: "Other (type your own)", label: "Other (type your own)", description: "",
			recommended: false, checked: false, role: "other",
		},
	]);
});

test("the done label is matched by suffix, not equality — its glyph is theme-dependent", () => {
	for (const raw of ["✔ Done selecting", "● Done selecting", "Done selecting"]) {
		expect(buildAskChoices([], 0, [raw], new Set())[0]!.role).toBe("done");
	}
});

test("navigation labels get their own roles so the card can footer them", () => {
	const choices = buildAskChoices([], 0, ["Next →", "← Back"], new Set());
	expect(choices.map((c) => c.role)).toEqual(["next", "back"]);
	expect(choices.map((c) => c.value)).toEqual(["Next →", "← Back"]);
});

test("an option absent from the spec still renders", () => {
	// A card with fewer buttons than omp offered is indistinguishable from a
	// broken card, so an uncorrelated label survives descriptionless.
	const spec = parseAskSpec(ASK_ARGS);
	expect(buildAskChoices(spec, 0, ["Kerberos"], new Set())[0]).toEqual({
		value: "Kerberos", label: "Kerberos", description: "",
		recommended: false, checked: false, role: "option",
	});
});

test("checked state is keyed by the CLEAN label and only reaches real options", () => {
	const spec = parseAskSpec(ASK_ARGS);
	const choices = buildAskChoices(spec, 1, ["Streaming", "Search", "Next →"], new Set(["Streaming"]));
	expect(choices.map((c) => c.checked)).toEqual([true, false, false]);
});

test("a title that disagrees with the spec degrades to descriptionless", () => {
	// Wrong-question descriptions are worse than none: a stale activeAsk must
	// not paint Q1's copy onto Q2's card.
	const spec = parseAskSpec(ASK_ARGS);
	expect(buildAskChoices(spec, 0, ["JWT"], new Set(), "Which features?")[0]!.description).toBe("");
	expect(buildAskChoices(spec, 0, ["JWT"], new Set(), "Which auth method?")[0]!.description)
		.toBe("Bearer tokens for stateless API clients.");
	expect(askQuestionMatches(spec, 0, "Which auth method?")).toBe(true);
	expect(askQuestionMatches(spec, 0, "Which features?")).toBe(false);
	expect(askQuestionMatches([], 0, "anything")).toBe(false);
});
