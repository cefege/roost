# plan — CONCLUSIVE fix: screen content can never break voice recording

file-owner: end the recurring "dictation dies depending on what's on
screen" class for good. Supersedes the whack-a-mole guards in
`plan/voice-keyterm-handshake-fix.md` (KTF1-KTF4) — those made the cliff
harder to fall off; this removes the cliff.

## THE DISEASE (why it keeps happening)

keyterms ride in the WS handshake URL (`?keyterm=...` per term in
`deepgramDictation.ts::buildUrl`). The URL is built from UNBOUNDED,
ADVERSARIAL on-screen content. Therefore:
  on-screen content → URL length/shape → handshake success/failure.
Deepgram enforces caps AT HANDSHAKE, before any audio → a bad/long URL =
1006 with no Error frame = recording never starts. Every new harness (pi
today, X tomorrow) paints a screen we didn't anticipate → back here.
The KTF guards each patch ONE bad-input shape. The structural fault is
that screen content can reach the thing that must succeed to record.

## THE CURE (two mechanisms, sequenced)

Decouple biasing from the handshake so screen content can NEVER prevent
recording — by construction, not by guessing tokens.

### phase-vkc-a — BOUNDED FIXED-SIZE URL (ships now, verifiable here)

Make the keyterm contribution to the URL bounded + predictable so the URL
is ALWAYS short enough to connect, regardless of screen content.
`deepgramDictation.ts`:

- `MAX_KEYTERM_URL_BUDGET = 1500` bytes — hard ceiling on the SUM of
  keyterm param bytes (well under the 8 KB request-line limit, leaving
  headroom for base params + token subprotocol).
- `MAX_KEYTERM_COUNT = 50` — hard cap on number of terms appended.
- buildUrl appends terms in rank order, stopping at whichever bound binds
  first (count OR byte budget). The existing 8000 trim stays as a final
  ASSERTION that is now unreachable in practice.
- Result: URL length variance is bounded to a small window; the handshake
  can no longer be pushed over any ceiling by a busy screen. Recording
  ALWAYS starts; worst case = fewer bias terms (cosmetic).
No Deepgram-API dependency. Fully unit-testable from here.
Test: adversarial 10k-term screen → URL length ≤ (base + 1500 + subproto)
and always < MAX_WS_URL_LEN; keyterms present but capped ≤ 50.

### phase-vkc-b — POST-CONNECT KEYTERMS — RULED OUT (Deepgram API limit)

VERIFIED 2026-07-09 via developers.deepgram.com/docs/keyterm (humanchrome):

- Nova-3 keyterms are URL query params ONLY. There is NO post-handshake
  keyterm channel for Nova-3.
- "Dynamic Keyterm Updates" via the Configure control message is **Flux
  ONLY** — not Nova-3. We run nova-3, so the full-decouple mechanism does
  not exist without switching models.
- Therefore the URL IS the only keyterm transport → bounding the URL
  contribution (phase-vkc-a) IS the conclusive fix for Nova-3. Nothing to
  decouple to. Plan collapses to one phase.
- BONUS: Deepgram explicitly recommends "the most important 20-50 terms"
  and "stay well under the 500 token limit." Our old cap (80 entries) was
  ABOVE their guidance → phase-vkc-a's 50-term cap aligns the bound to
  Deepgram's own recommendation, not just to a safe URL length.
- FUTURE ESCAPE HATCH (only if biasing quality ever demands >50 terms):
  migrate the model to Flux + send keyterms via the Configure control
  message post-onopen. Not needed for the connect-reliability fix.

## INVARIANT (the thing that must never regress again)

Screen content MUST NOT be able to prevent a recording from STARTING.
After phase-vkc-a this holds by URL-length bound; after phase-vkc-b it
holds because screen content isn't in the handshake at all.
Regression test asserts a pathological screen still yields a connectable
URL. If a future change reintroduces content-proportional URL growth, the
test fails.

## VERIFY

- unit: adversarial-screen URL-bound test (apps/web/tests) green.
- typecheck + lint-roost clean.
- live: /roost-smoke dictate into a pi session → recording starts, no 1006.
