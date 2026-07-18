// keytermContext — extract Deepgram Nova-3 `keyterm` biasing terms from the
// live terminal you're dictating into. Feeds deepgramDictation buildUrl so
// spoken project jargon (Kysely, tailnet, coordFactory) transcribes correctly
// instead of as phonetic mush ("kaiseley", "tail net").
//
// Method (the ranking): TF-IDF-lite — a term's weight = local frequency ×
// rarity (common-English stoplist) × code-structure bonus, summed over the
// visible grid (weight 1.0) and recency-decayed scrollback. Keyterm biasing
// only helps on tokens the decoder would otherwise mis-spell, so the whole job
// is keeping the rare/code-shaped tokens and dropping ordinary words.
//
// Budget: Deepgram caps keyterms at 500 tokens/request (aggregate). We expand
// each identifier to both raw + spoken form (deepgramDictation → "deepgram
// dictation") because the decoder biases toward however you actually say it,
// then greedily fill to the cap by score.
//
// Pure — unit-tested in keytermContext.test.ts. Caller: MobileVoiceInput.

import { STOP } from "./keytermStopwords.ts";

export interface TerminalContext {
	grid: string; // visible viewport — what's on screen now
	scrollback: string; // recent history, already capped to ~250 rows by the caller
	input?: string; // P2: user's own recently-typed tokens (highest signal)
	lexicon?: string[]; // P3: persisted recurring jargon (cold-start seed)
}

// Deepgram Nova-3 caps keyterms at 500 BPE tokens AND ~100 keyterm entries; over
// either, it resets the WS handshake (close 1006, no Error msg). Two guards:
// (1) token budget held WELL under 500 because tokenCount counts whitespace-words
// while Deepgram counts BPE subwords — a code identifier ("deepgramDictation") is
// 1 for us, several for them, so we must undershoot; (2) a hard emitted-entry cap
// (each ranked term emits raw + spoken = 2 entries) kept under the 100 limit.
const MAX_KEYTERM_TOKENS = 250;
const MAX_KEYTERM_ENTRIES = 80;
// Term-count ceiling before budget-fill — keeps the URL sane on a busy screen.
const MAX_TERMS = 90;
// KTF1-BYTE-BUDGET: the REAL binding constraint is on-wire bytes, not the BPE
// proxy. A pi harness paints dense/box-drawing/CJK rows Claude Code never did;
// their tokens inflate encodeURIComponent bytes past Deepgram's aggregate cap →
// 1006 handshake reset with NO Error frame (recording never starts). Sum the
// percent-encoded param bytes and stop here, well under the 8 KB request-line
// ceiling buildUrl enforces as a final backstop (KTF2-URL-LEN).
const MAX_KEYTERM_URL_BYTES = 4000;
// Oldest kept scrollback line weighs this much vs a grid line (newest ≈ grid).
const SCROLLBACK_DECAY_FLOOR = 0.3;
// User-typed input is the strongest signal: you say what you type.
const INPUT_WEIGHT = 2.0;
// Persisted-lexicon seed: enough to fill a cold pane, below live jargon.
const LEXICON_WEIGHT = 1.5;

const MIN_LEN = 2;
const MAX_LEN = 40; // hashes / base64 blobs aren't spoken

// One token's structural likelihood of being domain jargon worth biasing.
function structuralBonus(t: string): number {
	let b = 1;
	if (/[a-z][A-Z]/.test(t)) b += 1.5; // camelCase identifier
	if (/_/.test(t)) b += 1.2; // snake_case
	if (/[a-z]-[a-z0-9]/i.test(t)) b += 0.8; // kebab / nova-3
	if (/\d/.test(t)) b += 0.5; // version / numbered token
	if (/^[A-Z]{2,}$/.test(t))
		b += 1.0; // ALLCAPS acronym (WS, PCM, DB)
	else if (/^[A-Z][a-z]/.test(t)) b += 0.7; // Proper noun (Kysely, Deepgram)
	return b;
}

const EXT =
	/\.(ts|tsx|js|jsx|mjs|cjs|json|md|css|scss|sh|rs|py|go|txt|log|toml|ya?ml|html|sql|lock)$/i;

// Path → basename, strip a code extension, trim edge punctuation. A path token
// reduces to the spoken word ("deepgramDictation"), not "apps/web/src/lib/…".
function normalize(raw: string): string {
	let t = raw;
	if (t.includes("/")) t = t.slice(t.lastIndexOf("/") + 1);
	t = t.replace(/^[("'`[{<]+/, "").replace(/[)"'`\]}>.,;:!?]+$/, "");
	t = t.replace(EXT, "");
	return t;
}

function keep(t: string): boolean {
	if (t.length < MIN_LEN || t.length > MAX_LEN) return false;
	if (!/[A-Za-z]/.test(t)) return false; // need a letter (drop pure numbers/symbols)
	if (/^\d+$/.test(t)) return false;
	// Commit SHAs / hex ids: all-hex AND has a digit (so English words that
	// happen to be all hex-letters — "facade", "decade" — survive). Never spoken.
	if (t.length >= 6 && /\d/.test(t) && /^[0-9a-f]+$/i.test(t)) return false;
	if (STOP.has(t.toLowerCase())) return false;
	return true;
}

// KTF3-JUNK-TOKENS: `keep` gates SCORING; this gates what we actually APPEND to
// the WS URL. A term Deepgram would choke on (or that's never spoken, so useless
// as bias) must not reach buildUrl. pi renders control/box-drawing/spinner rows
// that survive `keep` but break the handshake. Reject a variant unless it's
// printable-ASCII-clean, mostly letters, and low-punctuation.
const PRINTABLE_ASCII = /^[\x20-\x7E]+$/;
export function isSpeakable(variant: string): boolean {
	if (!PRINTABLE_ASCII.test(variant)) return false; // control / box-draw / CJK / emoji
	const letters = (variant.match(/[A-Za-z]/g) ?? []).length;
	if (letters < 2) return false; // need real word content
	if (letters / variant.length < 0.5) return false; // mostly punctuation/digits
	const nonAlnumSpace = (variant.match(/[^A-Za-z0-9 ]/g) ?? []).length;
	if (nonAlnumSpace > 2) return false; // @//-heavy path/url residue — never spoken
	return true;
}

// How you'd SAY an identifier: split camelCase / snake / kebab / path into
// words. Returned only when it differs from the raw token. Both forms get fed
// so the decoder matches whichever you utter.
export function spokenForm(t: string): string | null {
	const split = t
		.replace(/([a-z0-9])([A-Z])/g, "$1 $2")
		.replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2") // RPCClient → RPC Client
		.replace(/[_/.-]+/g, " ")
		.trim()
		.toLowerCase()
		.replace(/\s+/g, " ");
	return split && split !== t.toLowerCase() && /\s/.test(split) ? split : null;
}

// Deepgram counts keyterm budget in SUBWORD tokens, not whitespace words: a
// raw identifier ("coordFactory", "node.js", "v1.2.3") tokenizes to several.
// Whitespace-only counting UNDERCOUNTS → on a dense code screen the real total
// crosses Deepgram's 500-aggregate cap and it drops the socket ("Keyterm limit
// exceeded"). Split on whitespace + punctuation + camelCase so our count ≥
// Deepgram's: overcounting just trims a few terms; undercounting breaks the
// connection. ponytail: heuristic, not Deepgram's exact tokenizer — being
// conservative is the whole point.
function tokenCount(term: string): number {
	return (
		term
			.replace(/([a-z0-9])([A-Z])/g, "$1 $2")
			.split(/[\s._/@-]+/)
			.filter(Boolean).length || 1
	);
}

const TOKEN_RE = /[A-Za-z0-9][A-Za-z0-9_./@-]*/g;

// Accumulator threaded through scoreLines — no module globals (CLAUDE.md r11).
interface Accum {
	scores: Map<string, number>;
	// Dominant casing per lowercased key: keep the variant with the richest
	// structure (proper-noun/camel beats all-lowercase) so we emit "Kysely".
	casing: Map<string, { term: string; bonus: number }>;
}

function scoreLines(acc: Accum, text: string, weight: number): void {
	if (weight <= 0) return;
	for (const raw of text.matchAll(TOKEN_RE)) {
		const t = normalize(raw[0]);
		if (!keep(t)) continue;
		const key = t.toLowerCase();
		const bonus = structuralBonus(t);
		acc.scores.set(key, (acc.scores.get(key) ?? 0) + weight * bonus);
		const prev = acc.casing.get(key);
		if (!prev || bonus > prev.bonus) acc.casing.set(key, { term: t, bonus });
	}
}

// Adjacent Capitalized-word pairs ("Cell Terminal", "Coord Link") — real
// product/domain phrases. Fed as multi-word keyterms so the decoder biases
// toward the whole phrase. Skipped when either word is a common stopword.
const PHRASE_RE = /\b([A-Z][a-z]{2,})\s+([A-Z][a-z]{2,})\b/g;

function minePhrases(acc: Accum, text: string, weight: number): void {
	if (weight <= 0) return;
	for (const m of text.matchAll(PHRASE_RE)) {
		const a = m[1]!,
			b = m[2]!;
		if (STOP.has(a.toLowerCase()) || STOP.has(b.toLowerCase())) continue;
		const phrase = `${a} ${b}`;
		const key = phrase.toLowerCase();
		acc.scores.set(key, (acc.scores.get(key) ?? 0) + weight);
		if (!acc.casing.get(key)) acc.casing.set(key, { term: phrase, bonus: 99 });
	}
}

/** Tokenize + score the live context into an accumulator (the expensive pass:
 *  matchAll over grid + ~250 scrollback rows + input + phrases). Does NOT fold
 *  the lexicon — keep that separate so the SAME accum can be finalized with and
 *  without the seed (learn-from-live vs seed-for-Deepgram) without re-scanning.
 *  Grid weighted 1.0; scrollback recency-decayed; user input highest. */
export function buildAccum(ctx: TerminalContext): Accum {
	const acc: Accum = { scores: new Map(), casing: new Map() };

	scoreLines(acc, ctx.grid, 1.0);

	const sbLines = ctx.scrollback.split("\n");
	const n = sbLines.length;
	for (let i = 0; i < n; i++) {
		const recency =
			n <= 1
				? 1
				: SCROLLBACK_DECAY_FLOOR + (1 - SCROLLBACK_DECAY_FLOOR) * (i / (n - 1));
		scoreLines(acc, sbLines[i]!, recency);
	}

	if (ctx.input) scoreLines(acc, ctx.input, INPUT_WEIGHT);

	// Multi-word phrases from the high-value sources (skip deep scrollback). Rare
	// (two adjacent Capitalized words) so a generous weight won't crowd unigrams.
	minePhrases(acc, ctx.grid, 2.0);
	if (ctx.input) minePhrases(acc, ctx.input, INPUT_WEIGHT * 1.5);

	return acc;
}

/** Rank + greedy budget-fill an accumulator into ≤500 Deepgram tokens. When a
 *  lexicon is given it's seeded onto a COPY (the base accum stays seed-free, so
 *  the learn-from-live extraction never reinforces the seed → no runaway). Each
 *  kept term emits raw + spoken form. */
export function finalizeKeyterms(acc: Accum, lexicon?: string[]): string[] {
	let { scores, casing } = acc;
	if (lexicon && lexicon.length > 0) {
		scores = new Map(scores);
		casing = new Map(casing);
		for (const term of lexicon) {
			const key = term.toLowerCase();
			scores.set(key, (scores.get(key) ?? 0) + LEXICON_WEIGHT);
			if (!casing.get(key)) casing.set(key, { term, bonus: 50 });
		}
	}

	const ranked = [...scores.entries()]
		.sort((a, b) => b[1] - a[1])
		.slice(0, MAX_TERMS)
		.map(([key]) => casing.get(key)?.term ?? key);

	const out: string[] = [];
	const seen = new Set<string>();
	let tokens = 0;
	let urlBytes = 0;
	for (const term of ranked) {
		for (const variant of [term, spokenForm(term)]) {
			if (!variant) continue;
			if (!isSpeakable(variant)) continue; // KTF3 — never append handshake-hostile junk
			const v = variant.toLowerCase();
			if (seen.has(v)) continue;
			const cost = tokenCount(variant);
			if (tokens + cost > MAX_KEYTERM_TOKENS) continue; // BPE-proxy backstop
			// KTF1 — the binding budget: on-wire bytes. "keyterm=" + percent-encoded value.
			const byteCost = 8 + encodeURIComponent(variant).length;
			if (urlBytes + byteCost > MAX_KEYTERM_URL_BYTES) continue;
			out.push(variant);
			seen.add(v);
			tokens += cost;
			urlBytes += byteCost;
			if (out.length >= MAX_KEYTERM_ENTRIES) return out; // Deepgram ~100-entry cap
		}
	}
	return out;
}

/** One-shot extract (tests + any single-pass caller). MobileVoiceInput uses
 *  buildAccum + finalizeKeyterms directly to tokenize once across learn+seed. */
export function extractKeyterms(ctx: TerminalContext): string[] {
	return finalizeKeyterms(buildAccum(ctx), ctx.lexicon);
}
