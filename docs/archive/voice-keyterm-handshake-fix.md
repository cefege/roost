# plan — voice keyterm handshake breakage on pi sessions

file-owner: fix the Deepgram dictation handshake reset triggered by
keyterms extracted from `pi`-harness terminal content. Callers of the
touched code: `MobileVoiceInput.tsx` (keyterms producer) →
`deepgramDictation.ts::buildUrl` (URL consumer) ← `CellTerminal.tsx`
readContext (grid/scrollback source).

## SYMPTOM

Dictation on an Roost pane running a `pi` session fails to START recording
on specific terminals. Was fine under Claude Code; regressed after switch
to the pi cloud harness. Not char-mangling — the WHOLE recording never
begins.

## ROOT CAUSE (analysis, not live-captured — user chose defensive fix)

keyterm string → `p.append("keyterm", t)` in `buildUrl`
(`deepgramDictation.ts`) → Deepgram WS handshake RESET before onopen.
pi paints a different grid than Claude Code (box-drawing, spinner frames,
ANSI-dense status rows, long tool-call blobs, CJK/emoji) → the extractor
(`keytermContext.ts`) emits tokens Claude Code never produced.

Four unguarded failure classes (KTF1-KTF4), none currently tested:

- KTF1-BYTE-BUDGET: `tokenCount` counts whitespace/camel splits; Deepgram
  counts BPE subwords + URL has a byte ceiling. Dense/CJK/emoji pi output
  inflates real BYTES past the 500-aggregate cap → 1006 reset, NO Error
  frame. Proxy count ≠ real budget.
- KTF2-URL-LEN: nothing caps final `wss://…` length. Busy pi screen → long
  URL → handshake reject.
- KTF3-JUNK-TOKENS: `keep()`/`TOKEN_RE` never tuned vs pi shapes — ANSI
  residue, `@`//-heavy tokens, lone high-Unicode "words". Never spoken
  (useless bias), still burn budget + bytes.
- KTF4-SILENT-MASK: `try { opts.keyterms?.() } catch {}` + 1006-no-Error
  path = dead recording, nothing actionable in logs.

## FIX (harden + real byte budget; feature stays ON)

### phase-ktf-a — sanitize keyterms (KTF3)

`keytermContext.ts`: add `isSpeakable(t)` gate inside the emit loop of
`finalizeKeyterms` (NOT `keep` — keep still feeds scoring). Drop a term
when it: contains any C0/C1 control or non-printable, contains box-drawing
/ block-element ranges (U+2500–U+259F), is not mostly ASCII-letters after
normalize, or has >2 non-alnum non-space chars. Spoken-form already strips
most; this guards the RAW form that also gets appended.
Test: hostile pi-grid fixture (box-drawing header + spinner + ANSI residue
- CJK path) → every emitted term matches `/^[\x20-\x7E]+$/` and is
speakable.

### phase-ktf-b — real byte budget + URL ceiling (KTF1, KTF2)

`keytermContext.ts`: replace proxy `MAX_KEYTERM_TOKENS` gate with a byte
accountant — sum `encodeURIComponent(variant).length` (approximates the
on-wire keyterm= param bytes) and stop at `MAX_KEYTERM_URL_BYTES` (budget
so total URL stays well under 8 KB; Deepgram/proxies reject long request
lines). Keep the BPE-proxy token cap AND entry cap as belt-and-suspenders;
whichever binds first wins.
`deepgramDictation.ts::buildUrl`: after building, if
`url.length > MAX_WS_URL_LEN` (hard 8000) drop keyterms lowest-score-first
until under. buildUrl already receives the ordered array (highest score
first) — trim from the tail.
Test: 400 long camel tokens → final URL byte-length ≤ 8000; keyterms
present but trimmed.

### phase-ktf-c — observability (KTF4)

`deepgramDictation.ts`: on WS-open `diag("voice.ws_url", { url_bytes,
keyterm_count, token_est })` (already have voice.keyterms — add url_bytes).
Replace the two silent `catch {}` around `opts.keyterms?.()` with a
`signal("voice.keyterms_failed", { detail })` so a future new-harness
regression is diagnosable from *.err.log. The 1006 closeMessage already
names network — add a hint that a busy/dense screen can trip the keyterm
cap.

### phase-ktf-d — verify

- `bun test apps/web/tests/keytermContext.test.ts` (new hostile-input cases
  green + all existing green).
- typecheck: `bun x tsc --noEmit` in apps/web scope (lens_diagnostics).
- lint: `bun scripts/lint-roost.ts` (no new raw values — pure logic, none
  expected).
- live smoke: `/roost-smoke` against the current tailnet coord host — open a
  pi session, dictate, assert recording STARTS (Deepgram onopen) and a
  transcript lands. (per CLAUDE.md real-flow floor.)

## INVARIANTS

- feature stays ON (user chose harden, not kill-switch).
- keyterm biasing must NEVER break the handshake: on any doubt, DROP the
  term, never append a risky one.
- pure functions stay pure + unit-tested (keytermContext.ts).
- no new store, reuse existing diag/signal facade.
