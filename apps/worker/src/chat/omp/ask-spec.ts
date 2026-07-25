// Correlate omp's `ask` tool arguments with the bare `select` frames it emits.
//
// omp's RPC transport flattens every select option to a label string
// (rpc-mode's RpcExtensionUIContext.select), so option descriptions, the
// recommended marker, per-question headers, and multi-select state never reach
// the wire. All of it IS on `tool_execution_start.args` for the ask call, which
// carries the unredacted tool input. This module rebuilds the render model from
// those two halves.
//
// Pure and structural: omp's types are deliberately NOT imported (same stance
// as rpc-driver.ts) and nothing here does I/O.

/** One option as the pane should render it. */
export interface AskChoice {
	/** Exact string to echo back in extension_ui_response — never a cleaned label. */
	value: string;
	/** Display label with the " (Recommended)" suffix removed. */
	label: string;
	description: string;
	recommended: boolean;
	checked: boolean;
	/** "option" | "other" | "done" | "next" | "back" */
	role: string;
}

export interface AskQuestionSpec {
	id: string;
	question: string;
	header: string;
	options: { label: string; description: string }[];
	multi: boolean;
}

/** omp appends this to the recommended option's label (ask.ts RECOMMENDED_SUFFIX).
 *  It is the ONLY way recommendation crosses the RPC wire. */
const RECOMMENDED_SUFFIX = " (Recommended)";
/** Reserved synthetic labels omp injects into an ask select list. */
const OTHER_LABEL = "Other (type your own)";
const NEXT_LABEL = "Next →";
const BACK_LABEL = "← Back";
/** The done option is `${theme.status.success} Done selecting` — the leading
 *  glyph is theme-dependent, so it can only be matched by suffix. */
const DONE_SUFFIX = "Done selecting";

function isRecord(v: unknown): v is Record<string, unknown> {
	return v !== null && typeof v === "object" && !Array.isArray(v);
}

/** Parse omp's `ask` tool arguments off tool_execution_start.args.
 *  Returns [] for anything that is not the ask schema — a partial
 *  reconstruction would put one question's descriptions on another's card. */
export function parseAskSpec(args: unknown): AskQuestionSpec[] {
	if (!isRecord(args) || !Array.isArray(args.questions) || args.questions.length === 0) return [];
	const specs: AskQuestionSpec[] = [];
	for (const raw of args.questions) {
		if (!isRecord(raw)) return [];
		const { id, question, header, options, multi } = raw;
		if (typeof id !== "string" || typeof question !== "string" || !Array.isArray(options)) return [];
		const parsed: { label: string; description: string }[] = [];
		for (const opt of options) {
			if (!isRecord(opt) || typeof opt.label !== "string") return [];
			parsed.push({
				label: opt.label,
				description: typeof opt.description === "string" ? opt.description : "",
			});
		}
		specs.push({
			id,
			question,
			header: typeof header === "string" ? header : "",
			options: parsed,
			multi: multi === true,
		});
	}
	return specs;
}

/** Split an omp select title into its parts.
 *  "(2 selected) Pick features (3/4)" → { question: "Pick features", progress: "3/4", index: 2 }
 *  "Which auth method?"               → { question: "Which auth method?", progress: "", index: 0 } */
export function splitSelectTitle(title: string): { question: string; progress: string; index: number } {
	// `(N selected) ` prefix on a multi-select re-prompt (ask.ts).
	let question = title.replace(/^\(\d+ selected\) /, "");
	// ` (i/total)` suffix inside a multi-question batch (NavigationControls.progressText).
	let progress = "";
	let index = 0;
	const match = / \((\d+)\/(\d+)\)$/.exec(question);
	if (match) {
		progress = `${match[1]}/${match[2]}`;
		index = Number(match[1]) - 1;
		question = question.slice(0, match.index);
	}
	return { question, progress, index };
}

/** Build the render model for a select frame.
 *
 *  `spec` may be [] (no ask call in flight, or a plain extension ui.select) —
 *  roles are still classified from the label alone and every option still
 *  appears, just without descriptions.
 *
 *  `expectQuestion` is the frame's own question text (splitSelectTitle's
 *  `question`). When it disagrees with the spec entry at `questionIndex` the
 *  spec is treated as unmatched: correlating the WRONG question's descriptions
 *  onto a card is worse than showing none. Pass "" to skip the check. */
export function buildAskChoices(
	spec: AskQuestionSpec[],
	questionIndex: number,
	options: string[],
	checked: ReadonlySet<string>,
	expectQuestion = "",
): AskChoice[] {
	const question = spec[questionIndex];
	const matched = question && (expectQuestion === "" || question.question === expectQuestion) ? question : undefined;
	const byLabel = new Map<string, string>();
	if (matched) for (const opt of matched.options) byLabel.set(opt.label, opt.description);

	return options.map((raw) => {
		const role = raw.endsWith(DONE_SUFFIX) ? "done"
			: raw === OTHER_LABEL ? "other"
			: raw === NEXT_LABEL ? "next"
			: raw === BACK_LABEL ? "back"
			: "option";
		if (role !== "option") {
			return { value: raw, label: raw, description: "", recommended: false, checked: false, role };
		}
		const recommended = raw.endsWith(RECOMMENDED_SUFFIX);
		const label = recommended ? raw.slice(0, -RECOMMENDED_SUFFIX.length) : raw;
		// A raw string absent from the spec still renders — a card with fewer
		// buttons than omp offered is indistinguishable from a broken card.
		return {
			value: raw,
			label,
			description: byLabel.get(label) ?? "",
			recommended,
			checked: checked.has(label),
			role,
		};
	});
}

/** True when `spec[questionIndex]` is the question this select frame is for.
 *  Callers use it to gate `header`/`multi` the same way buildAskChoices gates
 *  descriptions. */
export function askQuestionMatches(spec: AskQuestionSpec[], questionIndex: number, question: string): boolean {
	return spec[questionIndex]?.question === question;
}
