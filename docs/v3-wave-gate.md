# The wave gate checklist

Thirteen agents wrote into one coordinator crate at once, so a test binary
could not link while its crate did not build. Every slice therefore handed the
integrator its **mutation experiments in executable form** — the exact edit, and
the named test that must fail — instead of a caveat. This file is that list.

**This is a gate, not a wishlist.** Each row is a property a slice claims to
protect and a single line whose removal must break a named test. A row whose
test still passes after the edit is a test that does not test anything, and the
row stays open until it fails.

Run each mutation, observe the named failure, restore, observe the pass. Record
the result beside the row. **A row that has never been observed failing is
unverified, and "unverified" is the honest state — not "probably fine".**

**Two standing rules for running any row on this list.**

1. **Mutate a copy, never the shared worktree.** Copy the file byte-for-byte into
   a scratch directory, mutate the copy, run the real suite against it, re-sync
   afterwards. Two agents in this programme independently mutated in place and
   both paid: one had a read-only audit read a file mid-mutation and report a
   fixed security defect as **absent**, which cost a slice's credibility for
   nothing; the other had a stale poll report three already-restored files as
   still mutated, which cost a cycle and nearly cost a sibling's confidence.
   **The people most likely to mutate in place are the ones most confident they
   will remember to restore**, which is why this is a default rather than good
   practice. A copy is its own pin: there is nothing to coordinate and nothing to
   reconcile.
2. **A mutation proves a test bites. It does not prove the surrounding code
   compiles.** A slice's scratch crate gave `ChannelId` a stand-in carrying a
   `Display` the real type lacks, so ten `%channel_id` sites compiled in the copy
   and would not have compiled in the tree. Mutation evidence and compile
   evidence are different claims, and a gate needs both.
3. **`FnOnce is not general enough` is a producer problem, not a closure or a
   boxing problem.** The integrator guessed "missing `Pin<Box<_>>` in two places"
   and was wrong: boxing to `Pin<Box<dyn Future<Output = …> + Send + '_>>`
   compiles and does not fix it, because the `'_` is still the borrow. **A box
   does not erase a lifetime written into a closure's own return type.** The
   cause is that each attempt borrows its own subscription, so the future is
   `Future + 'a` for that borrow, and a closure cannot be generic over a
   lifetime it was not written to be generic over. The two escapes are to hand
   the future to something that **takes futures by value** —
   `FuturesUnordered`, `JoinSet`, an explicit `Vec` of boxed `'static` futures
   — or to make the future `'static` by giving it owned data. Binning it as
   "add a box" is the trap, because a box looks like the answer and is not.
4. **A per-file claim needs evidence produced by a command whose output
   actually contains it.** A slice reported "zero diagnostics in my files" from
   `cargo check … 2>&1 > /tmp/file` — the redirections in the wrong order, so
   stderr (where every diagnostic goes) went to the pipe and the file it then
   grepped held stdout, which for `cargo check` is nearly empty. **The claim was
   a grep over a file that never contained the diagnostics.** The same slice had
   two real errors in its files. `wc -c` on the artifact before believing a grep
   over it costs one second, and an empty artifact is a finding about the
   command, not about the code.
5. **A staleness claim must be shown, not asserted.** The naive rule — "if an
   owner says a diagnostic is stale, they are right" — is unfalsifiable and
   inverts into the failure it was meant to prevent: a number nobody can check is
   a number presented as a result. The falsifiable version costs one line:

   > An owner may report a diagnostic as stale **only by showing what the current
   > tree says** — the definition, or the file:line, read after the last fix
   > landed. "I looked and it is fine" is a claim; "here is the definition, and
   > here is why the access compiles" is evidence. An owner who cannot show
   > either re-applies the fix and lets the next compile settle it.

   This was earned: a downstream artefact reads as a *precise, field-level*
   error — naming a field, a type and a line — and is **less** trustworthy for
   being precise. Precision about a symbol is not evidence about a symbol.
6. **Know what your checks cannot see, and say so.** A slice that ran three
   negative controls and proved each one catches an injected defect still had
   none of them able to see a one-line *type* error: the parse check is blind
   because a type error is valid syntax, the arity audit because no bind is
   involved, the generated-message audit because no literal is involved. **"My
   checks are calibrated" is not "my checks cover this."** A control proves a
   check bites on the class it was aimed at; it says nothing about the classes
   it was not aimed at. The honest report states both, and the classes nobody
   covered belong to whoever runs the compiler.

## Two patterns the C1 wave produced, worth carrying forward

**An edit landing on one side of a boundary and not the other.** Every
compile error in the last two rounds of the coordinator integration, and both
of the field errors before them, was this and nothing else: a struct field that
moved while its literal did not, a signature that changed while its callers
kept the old arity, a return type that was declared rather than derived from
what the body returns, an alphabet that is not an engine. **None is a design
problem and none is subtle.** Each is found by reading the *current* text of the
other side, which is the thing people skip because they remember what they
wrote. A slice reported stopping its own trust in its memory of its own files
and reading them instead — that is the whole mitigation.

**Reusing a library constant can be a behaviour change dressed as a refactor.**
Every shipped `base64::engine::general_purpose::STANDARD_*` is documented *does
not allow trailing bits when decoding*, while v2's `Buffer.from(x, "base64")`
does allow them. Building the engine over `base64::alphabet::STANDARD`
preserves v2's accepted inputs; passing a shipped `STANDARD` engine back into
the constructor would have quietly narrowed what the coordinator accepts. The
compiler forced the question by rejecting the call — the engine is a
`GeneralPurpose` and the constructor wants an `&Alphabet` — and the answer was
not "pick the other spelling" but "check what each one does with the inputs".
**The general form: before reusing a dependency's ready-made value, check what
the version being replaced did, because a constant carries behaviour and the
type system will not tell you which behaviour you inherited.**

## The defect a shared crate with parallel writers produces most

**A parallel implementation of something that already had an owner — and it is
findable by searching for the *concept*, not the symbol.**

Four instances in one day, across three tracks:

1. A worker slice wrote its own `SnapshotPart` when the protocol crate already
   had one. It deleted its own rather than keep a second value.
2. The device-refusal helper exists **ten** times in the coordinator; seven are
   byte-equivalent and collapsible, three have genuinely diverged.
3. A pairing slice wrote `RequestOrigin` — structurally identical to M1's
   `CallerOrigin`, which had landed first. **It had already drifted on the one
   rule that matters**: its version had no notion of `X-Forwarded-For` at all
   and read `x-roost-remote-addr` as the client address, so a pairing request
   arriving through a front door would have recorded **the proxy's address as
   the requester's** — in exactly the field an operator reads beside the device
   they are approving. It also disagreed on whether a bracketed `[::1]` is
   loopback.
4. The auth slice asked whether a `SecureKeyStore` trait should exist at all
   rather than assuming the plan's shape was reachable.

**Why they are missed.** A slice searches for a *name* — `ListenerTrust`,
`CallerOrigin`, `SnapshotPart` — finds the type it was told about, and never
asks whether the concept is already owned. The reasoning that produces the
duplicate is always the same and always reasonable: *I was given this type and
this behaviour, so I will model it here, where I need it.*

**The check, which is cheap and was not run in any of the four cases:** before
writing a type that carries a behaviour, grep the crate for the *behaviour* —
what decides it, not what it is called. In case 3 the search that would have
caught it is `resolve_caller_origin` or `is_loopback_peer`, neither of which
contains the word "origin" in the shape a search for it would take.

**When a duplicate is found, cut over — do not wrap.** The pairing slice deleted
`RequestOrigin`, `from_peer`, `is_loopback` and `UNKNOWN_SOURCE_IP` outright and
made its helper a three-line delegation. A wrapper around a second
implementation is the same fork with an extra layer, and the layer is where the
next person stops looking.

## A compiler's error list across test binaries is a lower bound, and one
## generated-message shape is worth a mechanical sweep rather than a lesson

**A build that fails one test target can stop before checking the others**, so
the diagnostics a test run reports are not the set of errors in the test tree —
they are the set the compiler reached. A slice was handed two `E0063`s in one
file; sweeping its own six files found **nine** literals of that shape across
three files, seven of them in targets the run had never reached. The same
applies to a lib count: a fix that unblocks a later check is a *moved* error,
not a fixed one, so every residue count is a lower bound and never a total.

**A test has a TIER, and the tier is not visible in its name.** A slice's
capacity test calls `handle_agent_status_wait` directly. That covers the whole
chain inside its slice — registry refusal → `wait_error` → `ConnectError` — and
it would fail if the mapping were deleted or flipped. It does **not** observe
that the generated Connect service and the connectrpc runtime carry that code and
message to the client unchanged, and the test's name says "reaches the client",
which is a wire claim it does not make.

**So every gate row carries a tier, and a row that cannot state its tier is not
finished.** Handler-tier: the handler is called directly. Wire-tier: a request
goes through the generated `CoordinatorService` and its `RequestContext`, or
through the listener. A slice that cannot construct a wire-tier harness names the
row **unwritten** and says what would close it, rather than letting a handler-tier
pass stand in for it — which is exactly what this one did, and its saying so is
why the gap is visible at all.

**And the masking runs the other way: a failing TEST target hides a lib error
of the same shape.** A slice's test imported `PushNotificationTransport` from
`push::dispatch`; its source file did the same. The error list named only the
test, because test targets compile first and a test failure stops the run before
the lib is reached. Fixing the test would have revealed the same bug again one
file later, looking like new work. **Read the error list for the shape, not the
count: one mistake in two files is one mistake, and the list shows you one of
them.**

## `--keep-going` turns a lower bound into a total

The whole "an error list is a lower bound" rule above was true and **actionable
in exactly one way**: cargo stops at the first failing target unless you tell it
not to. The handoff said *13 errors, 1 warning, none in a file the previous lead
edited*. Re-measured with `--keep-going --message-format short`:

**56 distinct errors across 22 failing test targets, 17 source files. Zero in
`src/`.** One error — `tests/push_fixture/transport.rs:172` — was failing eight
test binaries on its own.

**So "13" was not a smaller true number; it was the set the compiler happened to
reach before it stopped.** Three numbers had circulated for the same state
(13, 27, 1) and all three were artefacts of the flag.

**The correction is the flag, not more care.** Any error count in a handoff is
unusable until someone re-measured it, and the re-measurement is one argument. A
number you inherited that cannot be reproduced with a flag is not a measurement;
it is a memory of where the compiler stopped.

## A binary that fails entirely is one defect, not N

The first diagnosis of 100 failures across 25 binaries: **several binaries fail
*entirely* — 7/7, 8/8, 9/9, 10/10 — and one at 1/11 and one at 1/7.** A binary
where nothing passed is a **fixture that never came up**, not N independent
defects. `pairing_confirmation` at 0/4 and `pairing_confirmation_authority` at
0/8 are almost certainly one shared fixture failing, and they are the same class
as the five missing `.await` calls and the test that seeded an empty database
and passed anyway.

**So the diagnostic order is: for every binary with a 0-pass count, find the
shared fixture and read it before reading any individual test.** Reading the
tests first is how a wave spends an hour on twelve independent defects that are
one. Sort by *shape of failure* — all-fail, most-fail, one-fail — not by count,
because the shape names the cause and the count does not.

And the same rule applies to a mutation: **a run that stops at the first
failure is a lower bound**, so mutation rows that "passed" because the run
aborted earlier are not passes. The integrator ran its six rows on a
**checksum-verified copy-and-restore harness** — back up, mutate, run, restore,
verify by sha256 — because a Rust mutation must compile in place, and *an
unverified restore is exactly the failure the copy rule exists to prevent*.

## The map, and the negative results that make it usable

Sorting the 100 failures by shape put **47 of them in two files**:

| module | binaries | tests | passes |
|---|---|---|---|
| `tests/agent_fixture/` | 5 (`agent_config_rpc`, `agent_status_ordering`, `agent_status_push`, `agent_status_rpc`, `agent_status_wait`) | 35 | **0** |
| `tests/pairing_support/` | 2 (`pairing_confirmation`, `pairing_confirmation_authority`) | 12 | **0** |

**The other four shared-module groups are explicitly NOT fixture problems**, and
that is the part that makes the map worth more than the count:

- `tests/terminal_view_support/` — 3 binaries, 1–2 failures each (6/1, 9/2,
  9/1). Most tests pass, so the module works and these are real assertions.
- `tests/tasks_support/` — `tasks_queue` has 3 failures while
  `tasks_refusals` is 8/0. **The shared module is fine.**
- `tests/mcp_relays_support/` — `mcp_relays_authority` 1, the other three mcp
  binaries fully green. A split-and-refactor survived intact.
- `tests/keeper_update_support/` — 3, 1, 0. Not a fixture problem.

**"They share a module" is the obvious guess and it is wrong in four of six
cases.** A negative result is worth as much as a positive one here, because the
wrong guess costs an hour and a positive one is visible on its own.

And the asymmetry inside one slice that changed the instruction: of A2's two
binaries, `bootstrap_single_use` (1/7) declares `auth_device_support` and is
therefore a shared-fixture case — but **`cf_access_identity` declares no module
at all and still fails 11 of 12.** So "A2's failures" is two unrelated causes in
two different subsystems, one of which holds the RSA key material. **Sending one
agent at a slice's failure count would have sent it to the wrong file for half
the work.**

## Establish the rule by running it, because the naive version breaks correct code

The 2018+ `use` rule looks like a one-liner and is not. A slice established it
**by running rustc on four minimal cases rather than by reading the edition
guide** — and it did so precisely because the obvious statement of the rule is
false:

A `use` declaration's first segment resolves against **what is in scope in the
module holding the declaration.**

| site | result |
|---|---|
| bare `use sibling::x;` at the **crate root** | compiles |
| bare `use sibling::{A, B};` inside a **function** of a root-level module | compiles |
| bare `use sibling::x;` inside a **child** module | **E0432** — needs `crate::` or `super::` |
| `crate::sibling::x;` | compiles |

**The discriminator is the module's position, not the name — and the rule "bare
first segment in a child module is wrong" is wrong.** There is a live site
(`deploy/keeper_update/refusal.rs:75`) with a function-scoped
`use ErrorCode::{…}` where `ErrorCode` is imported at that module's own top
level. **A blanket sweep would have rewritten it to a `crate::ErrorCode` that
does not exist**, and the coordinator came within one edit of exactly that. The
four rustc cases are the reason that site survived.

**So the question per site is "is this name in scope *here*", never "am I in a
child module"** — and the reason to run minimal cases rather than read the guide
is that a rule is only worth writing down once you have tried to write the short
version and watched it break something that works.

**One diagnostic, two unrelated fixes.** The same wave produced two different
E0432 causes at once: a private `use X as Y` where the consumers needed a
re-export, and a `#[path = "…"] mod renamed;` whose *child* then wrote
`use real_name::…`. Same diagnostic, unrelated causes, **both live in the same
fixture simultaneously.** A count that rises when you fix one says nothing about
the other.

## A shared test module must live in a subdirectory

Every `.rs` file directly under `tests/` compiles as **its own test binary**. A
shared module placed there becomes a binary that links nothing — it passes by
constructing an empty suite.

**So a shared fixture with more than one consumer must live in a
subdirectory**, declared by each consumer as:

    #[path = "keeper_update_support/mod.rs"] mod keeper_support;

and it reaches a sibling fixture with `use crate::workers_support::{…}`. The
path attribute and the directory are the same fact: the attribute is what lets
the file sit *inside* a directory while the module name stays flat, and both
fixture agents should converge on this shape if their consumers number more than
one.

## A mutation row needs both directions, and the pairing is not the numbering

Six rows arrived as one-line edits, each with the test that must fail. **Five
had no test named for the other direction** — the must-still-pass — which is the
gap that makes "this edit changed nothing" indistinguishable from "this edit
changed everything".

**The first correction here is mine and it is the one worth keeping.** Reading
the rows, I paired them by their *numbering* — M2+M3, M4+M5 — and wrote that
into this file. **The numbering is not the pairing.** Each must-still-pass was
embedded in prose inside its own row, so the two did not line up, and the real
relationships were:

- one row's must-still-pass is a test **in its own prose**;
- one row names a second test it *expects to fail* — **a must-fail, not a
  must-still-pass**, belonging to that row alone;
- three rows had no second direction at all.

**So: read the row, do not read the sequence.** I applied a pattern to a
numbered list and produced a confident wrong answer — the same error as resolving
a decode function from its name, and the reason the pairing question is
unanswerable by inspection.

**The rule, now with all six rows in it. A mutation row claims "this edit
breaks *this* property and nothing else", and it needs both halves:**

| | must FAIL | must STILL PASS | what the still-pass proves |
|---|---|---|---|
| M1 | a proof for a different action is refused | — | refuse-closed |
| M2 | a decision outside the drain is refused | a decision **inside** the drain is not refused **for the drain** | the guard is the restriction, not the drain being active |
| M3 | a replace-empty across a live session is refused | a force-live **does** cross a live session | the edit removed the *requirement*, not the path |
| M4 | the drain is held across the decision | a **second** preparation **is** refused while held *(a must-fail)* | what a concurrent update would do with the gate open |
| M5 | an unresolvable caller may not proceed | a preserved keeper **is** reported with its identity | the edit isolated the re-read, not the happy path |
| M6 | a shutdown reports no keeper because none is left | an authorized maintenance **may** cross a live session | the branch is about an identity being *present*, not about shutdowns generally |

**The must-still-pass is what proves the test isolates the property rather than
merely failing** — deleting a guard and seeing both tests go red tells you the
guard was load-bearing; seeing one go red and the other stay green tells you the
second is pinning the *absence* of the refusal, which is a weaker property than
its name claims. **If the named must-still-pass also fails, that is reported,
never adjusted.**

## A numbering that runs out is a silent scope loss

A seventh check — that the outbound frame is really the `KeeperUpdatePrepare`
arm and not a `BrowserCommand` wrapper carrying JSON, which is exactly what a
sloppy port reaches for to route around a missing enum arm — was described in
prose and never numbered. It needs a `BrowserCommand` fake; the six numbered
rows do not.

**Two checks were momentarily one, and the consequence would have been that the
fake-requiring check was dropped from the schedule with no trace** — a check
that is harder to write is exactly the one that disappears when it is merged
with an easier neighbour.

**So: a list of experiments has to have a stated cardinality, and anything
described but not enumerated must be either numbered or explicitly parked.**
"Here are six" and "here are six and a seventh I did not number" are different
claims, and only the second is safe.

## The defect the fixture blamed, and the one it hid

Two binaries, 12 tests, **0 passes**, sharing one module. The rule sent the agent
to `tests/pairing_support/mod.rs` first. **The fixture was correct.** The defect
was in `src/`: `pair_requests.id` is `TEXT` in both migrations and in v2
(`pairing-account.ts:139` inserts an ephemeral id), while **five Rust
declarations typed it `i64`.**

**And the shape rule is what found it.** Had the reading been *"0-pass binary,
blame the fixture"*, this would still be broken in production today — the fixture
is what the tests *call*, and it is the callee that is wrong.

Two consequences, and **they are different defects from one wrong type**:

- every `read_pair_request` — PairCreate's retry path, PairApprove — **errors
  out**, loudly.
- every `LiveSelector::ById` terminalize **binds an integer against a
  TEXT-affinity column and matches zero rows**, silently.

**The second is the dangerous one: the query is valid, it returns an empty set,
and nothing says that is wrong.** A compile cannot see it. A green suite built on
a fixture that never ran could not see it. **The only reason it surfaced is that
the tests ran at all and failed loudly** — which is the whole argument for
executing a new target before believing anything about it.

The fix went the right way: **the Rust types change, not the schema.** v2 parity
is the schema's job, and making a wrong Rust type compile by changing the schema
would trade a visible type error for an invisible migration divergence from the
port of record.

**And the standing question after a type fix lands: did the mask hide a second
cause?** A binary that moves less than its shape promised has told you something.
The same applies to the silent half: **if a selector is supposed to match rows
and a test passes because it matched none, that is a green-but-vacuous test** —
and the question is the one from `Scratch::second_core()`: *what made this test
able to fail at all, and is that still here?*

## A test mis-written against a fixture with no seam is fixed at the FIXTURE

`a_wrong_requester_token_finds_nothing` had a name, a doc comment and an
assertion message that all said *a wrong token* — and it called
`fixture.confirm(CODE, …)`, where `confirm` **hardcoded the correct requester
token.** So the test asserted that a *successful* confirmation equalled
`Err("not found")`, while its own final line asserted no key was authorized.
**It could never have passed, and its last line contradicted its first.**

The cause is not a wrong assertion. It is that **the fixture offered no way to
present a different token** — there was no seam, so the test was written against
the only call available and then described as something else.

**So when a test's name and its call disagree, widen the fixture rather than
rewrite the test:** add `confirm_with_token(token, code, now_ms)` with `confirm`
delegating to it, and the property becomes *strictly stronger* with no assertion
weakened. Rewriting the test to match the fixture would have made it green and
kept it vacuous.

This is the `Scratch::second_core()` rule from the other direction: there, a
property could be lost in a merge; here, a property was never expressible, and
the test compensated by lying about what it exercised.

## A test that cannot construct its own input is not a test

Three failures in `tests/pairing_provenance.rs`, and in all three the test dies
**before reaching the product logic**:

- a header name with a trailing space → `InvalidHeaderName`
- a value of `"Ottawa\r\nX-Injected: 1"` → `InvalidHeaderValue`
- a 60 000-byte `"é"` → `HeaderValue::to_str()` rejects non-visible-ASCII, so
  `read_bounded` returns `None` and the test panics on its own helper

**The second one is a header-injection test.** Its entire purpose is to prove
the product refuses an injected header, and it cannot construct the string
`http` refuses to build. **The test asserts nothing about the product, and it
fails, so the count looks like a defect when the defect is in the test's own
scaffolding.**

So a panic on a *test helper's* line is categorically different from a panic on
an assertion, and the diagnostic order says so: **read the line number before
the assertion message.** A failure inside the fixture's own construction is a
harness defect, and no amount of product fixing will move it.

**And the honest way to report "I cannot tell whether this pre-existed":** an
agent tried to baseline a neighbouring binary by stashing its own changes, hit
the shared cargo lock, **aborted, popped the stash and verified the tree
restored** — then said *pre-existing by construction, not by baseline
measurement.* Declining to produce a number you could not obtain, and naming the
attempt, is worth more than the number would have been.

## A check that quietly stopped looking is worse than one that is noisy

The noise rule already on record: *a sweep that is mostly noise is a false
assurance — discard it.* This is the **more dangerous** version, and it came from
an agent falsifying its own prediction rather than waiting to be right.

A scan of seed SQL against the migration flagged 2 candidates. Both were hand
checked and both were false positives — **artifacts of the scanner splitting on
commas inside `--` comments.** So the scanner was hardened. And the hardened
version **stopped splitting wrongly and began missing statements: 1 checked
instead of all.**

**So the instrument was discarded rather than reported, and the number that
stood behind was the FIRST scan's: 19 modules, 2 candidates, 2 hand-verified
false positives, no real unknown-column defect in any shared fixture.**

**A tool that gets louder is easy to notice and a tool that gets quieter is
not.** A noisy check produces output you can dismiss; a check that has silently
stopped covering its input produces a clean result indistinguishable from a real
one, and a clean result is exactly what a gate wants to hear. **The signature is
a count that changed for a reason nobody asked about** — here, "1 checked
instead of all", which is the only thing in the report that would have given it
away.

**So the rule has two halves, and the second is the one that catches defects:**

- a check that is **noisy** is discarded, because it cannot be acted on;
- a check whose **coverage changed** is discarded, because its result now
  describes a different set of inputs than the one you asked about.

And the standing number is named as *"the first scan's"*, with the caveat
attached, rather than a single confident figure. **An agent that narrows its own
claim and says which run it is standing behind has produced a more usable fact
than one that reports the best number it obtained.**

The same agent's static sweep of the other class — every `pub const … : &str`
across all 19 fixture modules, validated against its brand rule — returned
**19 fingerprint-shaped constants, 27 UUID-shaped, 0 violations**, and
`agent_fixture` was the only fixture in the crate that could not satisfy
`WorkerFp::check`. **The defect existed exactly once.** And with both shapes
checked, its prediction stayed falsifiable rather than settled: *if the run shows
a shortfall, the cause is something neither scan covers* — a dropped `.await`
leaving a seed empty, which no static scan can see, or a `Drop` that discards an
error. **Those need a run to see, not a grep.**

**So the rule has two halves, and the second is the one that catches defects:**## Both kills were one defect: a value of the wrong SHAPE at a boundary that cannot reject it

The two 0-pass fixture groups produced **two production defects, and they share a
cause**:

- 32 hex characters where a 64-hex branded type is checked;
- `i64` where the column is `TEXT`.

**Both are the right type in the wrong place, and the compiler enforces
neither** — one is a valid string, the other a valid integer, and both are
wrong only at a specific boundary that cannot tell.

**A panic in a fixture and an empty result set from a live query are the same
defect in different clothes.** And the silent one is the one that ships:
`LiveSelector::ById` matched zero rows and returned a **valid empty set**, which
to every caller is indistinguishable from the right answer. The loud one — a
panic in a test constructor — is the one that gets found, because a test that
cannot run is impossible to ignore.

**So the diagnostic question is "what is this boundary unable to reject", not
"is this value valid".** A brand type is only as good as the construction that
enforces it, and a constructor that panics on bad input is a boundary that
*does* reject — which is why the fixture failure was the good news.

**And a fully implemented, fully tested method can still never run once.**
`settle()` deregistered the waiter before awaiting: `WaiterEntry` owns the
`oneshot::Sender`, `remove()` dropped the registry's `Arc`, the local one died
before the `.await`, and **every `AgentStatusWait` not already satisfied
returned `Canceled` instantly.** Nothing about that is subtle, nothing about it
would have been caught by a compile, and a method can be implemented, tested at
the seams either side of it, and still have never executed.

**And the rule bit its author within a day, in the direction that flattered
him.** A published "97 binaries" came from `grep -c "^test result"` — which
**also matches the trailing summary line**, so the real figure was 96. The fix
adopted was a *mechanism* rather than a promise: derive the map from a file,
print the file's byte size, its `Running` count and its `test result` count
beside the number being reported, and **let the agreement be the check**.

**## Transfer a rule as the failures, not as the caution

Four fresh agents were about to be given the measurement rule. The instruction
that was going to work is not "be careful with numbers" — it is **the five
specific ways the number went wrong in this tree**:

`13 errors`, `27 errors`, a `97 binaries` from a `grep` that also matched its own
summary line, six binaries lost to a truncated printout, and a stale `581/38`
that two clean runs falsified.

> **An agent told only "be careful with numbers" will be careful. An agent told
> the five ways it went wrong here will recognise the sixth.**

**A general caution produces generic caution, which is indistinguishable from
compliance and does not survive a surprising situation.** A specific failure
produces recognition, because the agent now has a *shape* to match against — and
the shapes here are not alike: a count that stopped early, a count that included
the summary, a count that was cut off, and a count that was true when taken. An
agent shown those four shapes will notice a fifth that shares none of their
mechanics.

**So the rule for handing a hard-won lesson to a fresh agent: name the
instances, not the principle.** The principle is the *result* of the instances
and it is strictly weaker to hand over on its own.

**And the corollary, for classifying a known class:** when an agent is given a
class that is already diagnosed, it must be told that **finding a thing that is
not the class is a finding, and a loud one — because a mis-attributed class is
worse than an undiagnosed one.** A mis-attribution makes the real defect look
explained, and the class's own name is what makes it believable.

## A defensible decision that removes a check, and records only the decision

`bun_abi` is carried by the proto message and **deliberately left unmapped** in
the Rust contract, with the reason in the file header: *a v3 keeper is not a Bun
process, so there is nothing truthful to put in it.*

**The reason is sound** — writing a Bun ABI string for a Rust binary would be a
lie in a field whose whole job is to be believed.

**But v2 uses that field for a restart-admission decision:**
`target.bun_abi === running.bun_abi` refuses a restart whose contract disagrees
with the running one. So the port has silently dropped a check, and **the only
record of that is a comment explaining why the field is empty** — which is a
record of the *decision*, not of the *consequence*.

**So the general form: when a port decision removes a check the reference
performs, the commit body must name the removed check, not only the reason the
field is now empty.** A reader who finds the comment learns the field is
unmapped; a reader who needs to know *what admission rule no longer exists* has
nothing to find. And the removed check is invisible in a diff, because a missing
check and a never-implemented one are the same shape.

This was recovered from an agent transcript: the agent found it, concluded "this
is a product defect", and was **hard-aborted before writing it down.** The
finding survived only because the transcript was still readable — which is the
argument for treating an agent's transcript as a durable artefact rather than a
stream.

## A hard-aborted agent's conclusion is still a conclusion

An agent killed mid-turn had reached a product-defect call and had not recorded
it. Its integrator correctly refused to reconstruct the verdict, and refused to
promote the *symptom* it had gathered — three tests sharing one refusal — to a
cause, **because a shared symptom is exactly what a shared fixture and a shared
product bug both look like.**

**Both refusals are right, and together they are the method:** do not reconstruct
an unread verdict, and do not promote a symptom to a cause. What is left is to
re-run the experiment with a fresh agent — and separately, to read the dead
agent's transcript for the *leads* it gathered, which are evidence and were never
claimed as conclusions.
## The checksum proves YOU restored. It says nothing about anyone else.

The per-row harness printed sha256 before and after every mutation and required
them equal — and **two mutations were still found sitting in a live tree**, left
by an agent that was hard-aborted between applying an edit and running the row.

**Both were identified only by reading their diffs** (`delete the
&& !open_session_ids.is_empty()`; `delete reauthorize_device(...)`) and reverted
by hand, and **neither was detectable from the harness**, because the harness
had no way to know a second writer existed.

> **The checksum proves *you* restored correctly. It says nothing about whether
> *anyone else* touched the file, and in a shared worktree those are different
> questions.**

So the sound check is the conjunction, and only the first half was enforced:
**the digest was equal at both ends AND no one else wrote the file in
between.** A per-row harness owns the first; **only the absence of concurrent
writers owns the second**, and the cheapest proxy for that is a mutation window
that is announced and *kept* — because a dead agent in an unannounced window
leaves a tree that looks edited on purpose.

**And the practical rescue, which is worth more than the rule: a mutation is
recognisable from its diff.** A deleted guard, a deleted call, a deleted
condition — a mutation reads as a suspiciously small removal, and that is a
shape a reader can catch in one glance. **The dangerous artefact is not the
mutation; it is a mutation that has been committed.**

## A row that confirms a fix is the strongest shape a row has

The staleness fix went in and **the same row was re-run on the green tree**:
`update.common.revision > held.common.revision` → `true` still fails
`a_late_report_never_displaces_a_fresh_one` with `left: Accepted, right: Stale`.
Restores checksum-verified on both runs.

> **The row ruled the line out on the broken predicate and confirms the fix on
> the good one — and the second run is the one that says the fix did not merely
> move the failure somewhere else.**

**Three verdicts, in increasing strength, and they are not interchangeable:**

| verdict | what it establishes |
|---|---|
| **BIT on a red test** | the guard is load-bearing, and *nothing* about isolation |
| **BIT on a green test** | the guard is load-bearing and the test isolates it |
| **BIT on green, before and after a fix** | the guard is load-bearing, the test isolates it, **and the fix did not relocate the failure** |

**The third is the one that closes a defect rather than describing one**, and it
costs one extra run. A fix that moves a failure from one assertion to another in
the same binary passes a naive "is it green now" check; **a row that bit before
and after is the only thing in the set that distinguishes a fix from a
relocation.**

**And the bracket did what a bracket is for.** The third hypothesis was
necessary-and-insufficient — it fixed the case it was written for and broke the
other — and that is not a failure, it is **the answer located between two
bounds**: the rule, plus the dimension the two test bodies share. The dimension
turned out to be a number in a fixture, not a relation between values.

> **You do not need a fourth guess after a necessary-and-insufficient result. You
> need one read, and the bracket has already told you where.**

Three dead ends were recorded in the commit body with the test that killed each,
and **all three were model errors rather than typos**: every line they touched
was correct, and every line the row can delete is correct. **A dead end that
touched only correct lines was a wrong model, not a wrong edit** — which is the
cheapest possible dead end and the most informative one.

## The axis is the REVISION, and it is a property of neither side

Four hypotheses. The first three assumed the missing rule was a **property** —
of `held` (is it legacy?), of `update` (is it identified?), or of the pair (is
held-legacy-and-update-identified?). The fourth found it is none of those:

> **The axis is the revision, and it is not a property of either side.**

- An identified frame at **revision 1** is a **brand-new occupant** numbering
  its first report from scratch, and a new occupant legitimately takes over.
- An identified frame at **revision 2 or above** is a **continuation** arriving
  for a session a legacy agent still holds, and must be refused as `Stale`.

Before the fix, both fell through to the same `return update.active`, **so the
continuation silently took the session.**

**So the general form, and it is the reason three property-shaped hypotheses all
failed:**

> **A missing condition is often a *relation* between two values, and a relation
> is not discoverable by asking what either value is.** Asking "is `held` legacy?"
> and "is `update` identified?" are both answerable from the code and both
> insufficient; only *"how do these two relate along a third axis"* is the
> question the predicate was actually failing.

**And the failure mode of the three attempts is worth naming, because it is
general:** *a tightening changes one side of a relation, and a direction is not a
tightening.* Making the legacy branch stricter moved the boundary between the two
cases without moving the boundary in the direction that separates them — so it
passed one test and broke the other, every time, in a minute.

**The fix is a pure addition — 34 lines added, 0 deleted — and the comment
records both wrong attempts and why they were wrong.** That is the part that
makes the next reader's minute unnecessary: *a tightening changes one side of a
relation, and a direction is not a tightening*, written next to the line that
finally works.

## Read the test BODY. An assertion message is a hypothesis about the setup.

Three hypotheses were tried against one predicate. All three failed, and the
third failure is the useful one — because the two tests turned out to be **in
direct conflict on the very shape the third hypothesis proposed**:

- held **legacy** → update **identified** at **revision 1**, expecting **ACCEPT**
- held **legacy** → update **identified** at **revision 6**, expecting **REFUSE**

**Same relation, same direction, opposite expectations — distinguished only by
the revision number, and nothing in the predicate says `1` is special.** So the
missing rule is not the relation that was proposed; the relation is real and
**insufficient**, and the defect **cannot be described accurately** until the
axis is named.

The agent had been **reading the assertion messages and inferring the setup from
them** — and the test named *"a legacy frame yields permanently once an
identified occupant is accepted"* is not exercising a legacy frame at all. **The
name and the message were both describing something other than the body.**

> **An assertion message is a hypothesis about the setup, not a description of
> it. It is written to be readable on failure, which is a different purpose from
> describing what the test does.**

**So the read that resolves this is one test body, end to end** — and the
generalisation of the whole class is now three instruments deep:

- a **code-reading pass** cannot find it, because the code is right;
- a **mutation row** cannot find it, because every line it can delete is correct;
- an **inference from an assertion message** cannot find it either, which is the
  new instance.

**Only the test body carries the axis**, because the axis lives in the fixture.

**And the stopping rule, which is the part worth keeping:**

> **Three hypotheses failing in a row is evidence that the model is wrong, not
> that the fourth guess will be right.** A fourth attempt would be a fourth guess
> at a shape now demonstrated not to separate the cases.

The agent said it had had *three opinions about a function it had not read end to
end*, and asked for the read rather than taking it. **That is the whole
discipline in one sentence**: the number of failed hypotheses is not diligence, and
past a point the honest next step is a read rather than another edit. **A count of
attempts is not evidence of care.**

## The defect was a MISSING RULE, not a wrong branch

Two hypotheses aimed at the same line and both were wrong, because the line was
correct. What the failing test actually exercises:

- held **legacy** at revision 5, update **identified** at revision 6 → expected
  `Stale`, and the comment says why: *"once an identified occupant exists, a
  legacy one can no longer answer for the session."*
- walked through the predicate: the update is identified so the legacy-*update*
  branch is skipped; `is_retired` false; `previous` is `Some` so the `else` is not
  taken; and the *held* row is legacy, so `agent_status_identity(&held).is_none()`
  is true → `return update.active` → `Accepted`.

**So the line is on the path AND is correct — which is exactly why tightening it
broke the other test.** The two tests pin **opposite directions through the same
line**:

| held | update | required |
|---|---|---|
| identified | legacy | **accept** |
| legacy | identified | **refuse** |

> **The predicate cannot tell those apart.** The occupant check is false for a
> legacy held row, so two different rules fall through to the *same*
> `return update.active`. **The rule that is missing is a *direction* test —
> legacy-held versus identified-update — and it exists in none of the returns.**

**So the general form, and this is the fourth distinct shape in one predicate:**

> **Two branches are not "the same line twice". They are two different rules that
> happen to share a return value, and the thing that distinguishes them is the
> pair of tests that pin the pair of directions — not the code.**

A code-reading pass cannot find this, because the code is right; a row cannot find
it, because every line it can delete is correct; and a reviewer comparing two
identical branches sees a pair. **Only asking "which direction does each test
require?" exposes an absent condition**, and the answer is a *relation* between
two values rather than a property of either.

**And the disposition was right a third time: the change came back as evidence
rather than as an edit**, with the note that it has now had two wrong hypotheses
in this predicate and the change is *checkable against both pinned tests before
it is written.* **That is the cheap test applied to a behaviour change** — a
hypothesis that two existing tests can falsify in a minute should meet them
before it meets the compiler.

## A fix a test caught is cheaper than a fix a reviewer caught

One behaviour change was authorised on a two-line hypothesis — the legacy branch
of a staleness predicate returned `update.active` unconditionally, and a row had
already bounded its *neighbour*, so the defect "must" be there.

**It compiled, and the very next run said otherwise in under a minute.** A test
named `a_legacy_frame_yields_permanently_once_an_identified_occupant_is_accepted`
failed with `left: Stale, right: Accepted` — i.e. the branch **is** deliberate and
**already had a test pinning it**, exactly like the sibling. The change was
reverted, `git status` clean, the binary back to its prior count.

**And the original failure was still there with the change in place** — so the
hypothesis was not merely wrong, it was wrong *and* irrelevant, which is the
cheapest possible way to be wrong.

**What it bought is a tighter bound, not a fix.** Of four returns, three are now
ruled out: one by a mutation row, one by a passing test, one by the test that just
failed. **The search area went from "the predicate" to two lines**, and
*"one disproved fix is enough for a day"* is the right stopping rule.

**So the rule, which applies to every behaviour change on a shipped path:**

> **A fix that a test catches is worth having, and a fix that survives review is
> worth more. The first outcome is information; the second is a liability.**

The general shape: **a hypothesis cheap enough to test in a minute should be
tested in a minute, not argued in a review.** The cost of being wrong here was
one reverted line and one run. The cost of the same wrongness argued rather than
tested is a shipped behaviour change nobody can distinguish from the fix.

**And the identical-branch rule needed its other half, which is the better half:**

> Two branches written the same way whose *reasons* differ are indistinguishable
> from correct code — **and two branches written the same way whose rules are
> both deliberate are indistinguishable from a wrong copy.**

**The thing that distinguishes them is the test each one owns, not the code.**
Here both early returns turned out to be deliberate, each with a test naming it,
and the defect was in neither. **Identical code is not a smell on its own; the
question is whether each copy is pinned by a test that says which rule it is.**

## Two branches written identically, where only one of them has a reason

A staleness predicate with three returns, and a mutation row proved the **last**
one was guarded while the predicate still accepted a stale report:

```
:115  if agent_status_identity(&held.common).is_none() { return update.active; }
:118  if !same_agent_status_occupant(...)            { return update.active; }
:120  update.common.revision > held.common.revision
```

**Both early returns `return update.active` without comparing revisions at all.**
And they are written identically.

**`:118` is correct and deliberate, with a test pinning it:** a replacement
occupant numbers its revisions from 1, so refusing it as stale would strand the
session. **`a_replacement_occupant_is_not_a_stale_report_and_retires_the_previous_one`
is exactly that test.**

**`:115` has no such justification.** A held row with no identity is a *legacy*
record, and on that path the function accepts any active update at any revision —
including one at or below the held revision.

> **The occupant-change branch has a reason and the legacy branch does not, and
> they are written identically — which is precisely why the row could rule out
> the third line and leave the defect standing.**

**So the general form, and it is a review question rather than a code one:**

> **When two branches are written the same way, ask what each one is FOR. If one
> has a justification and its twin does not, the one without it is either a bug
> or an undocumented decision — and it is indistinguishable from the other in a
> diff, in a read, and to every tool that checks behaviour rather than intent.**

A reader scanning the predicate sees two early returns and reads them as a pair.
**The pair is the camouflage.** Two branches that differ in *why* must not be
written identically, because identical code carrying different reasons is
unreviewable — and the row that could have caught it was bounded to the line that
*was* correct.

**And the disposition is right: a behaviour change on a shipped path is named in
the commit body, not made as a side effect of a test failing.**

## A row that BITES and a defect that PERSISTS are compatible, and the pair is the finding

Three pre-registered rows, written long before this wave, all three bit — and
**every one of them fired on a test that was already red for a second reason**:

- **C5 BIT** on `status_order.rs:120`, and the live staleness failure is a
  *different* test entirely. So **C5 biting rules `:120` out as the cause**, and
  the defect is in one of the two earlier early-returns in `accepts`. The row
  bounded the last of three; the defect is in the first two.
- **C10 BIT**, failing at `:53` while the same test is currently red at `:81` —
  the meta defect is genuinely guarded, and the live failure is something else
  in the same test.
- **C11 BIT**, but measured against red, and correctly labelled as such: the
  verdict is real and **the isolation is not established**.

> **A test whose row bites and which is also red is failing twice over, and the
> row cannot tell you which failure you are looking at.**

And the finding that generalises past this wave:

> **The gate's instrumentation is working and the failure set has grown
> underneath it.** The rows are not stale — the tests have acquired a second
> cause.

**This is a different problem from stale rows, and running the rows one at a
time and recording BIT would never have shown it.** Three BITs that look like
three successes are, together, three tests with an unexamined second defect each.
**The pair — a row that bites plus a failure that persists — is strictly more
informative than either alone**, because one bounds what the guard covers and the
other says what is still uncovered.

**And the labelling rules this forced, all three of which are now standard:**

- **A mutation that does not compile is INCONCLUSIVE, not "did not bite".** They
  are different verdicts: "did not bite" is a claim about the test, and a
  compile error is a claim about the edit.
- **A row measured against a red baseline is BIT-with-unestablished-isolation**,
  and must be labelled that way rather than folded into a pass.
- **When a row bites a red test, the row's job changes.** It is no longer
  answering "is this test sensitive"; it is answering "what else is wrong with
  it", and the second question is the one the wave needed answered.

## The row that already exists for this defect is cheaper than diagnosing it

Three `tasks_queue` failures were read as **three individual assertions** by the
map that classified them. They are **one product cause**: three independent
handlers, **zero deliveries**, same module, different tests. `left: []` where
`[Created, State, State]` is expected — not a wrong delta, not a wrong order, **no
deltas at all**. That is a bus that is not connected.

**And the mutation row for it was written months ago and has never been run.**
`C11` — *delete the `publish` call in `handle_tasks_set_state`*, must fail
`a_task_change_reaches_a_sync_subscriber` — is **precisely** the row that catches
this class. The same is true of `C5` for the staleness defect below.

> **Before diagnosing a defect by hand, check whether a row already exists for
> it. A row is a cheaper experiment than a reading, it has a pre-registered
> prediction, and it produces a control the reading cannot.**

**The corollary, and it is the sharper half: a defect whose row has never been
run is a defect that has never been *bounded*.** The row does not only confirm
the cause — **it states how much of the surface that one `publish` call owns**,
which is information no amount of reading the failure would give you.

## Four product candidates, two of them the shape that has destroyed contracts

Of 14 remaining failures, four are product candidates rather than test defects:

- **A staleness check that accepts a stale report.** `left: Accepted, right:
  Stale` — a report that should be refused is accepted. **This is the most
  consequential single failure in the list, because it is the difference between
  the last word and the first**, and it is the same `status_order::accepts`
  predicate `C5`'s row guards.
- **A busy store reported as `Internal` where `Unavailable` is expected.** The
  test's own message is the argument: *"the call is retryable, so it is neither
  the caller's fault nor a statement failure."* **A busy store is a
  `Unavailable`, and reporting it as `Internal` tells an operator the coordinator
  is broken when it is merely saturated.** A caller that treats `Internal` as
  non-retryable will not retry a request that should be retried.
- **The unconnected bus** above.
- **A deletion that leaves the row present and misordered** — the same family as
  the staleness defect, in the same ordering logic.

**All four are the same shape: a boundary that reports the wrong thing, so the
observable the contract promised is simply absent.** Something upstream errors, or
does not run, and the error either disappears or is reported as the wrong kind.
**This is the third occurrence of that shape in this port** and the second today.

**And the discipline that produced them: the agent refused to classify the four
it had not read**, pointing out that the map's "individual assertions" label was
wrong about `tasks_queue` too. **A classification is not inherited — it is
re-earned per cluster**, and a map's verdict on a cluster nobody has opened is a
guess with a number on it.

## The first complete mutation rows — and the two that did not behave

Six rows, run with both directions and a checksum-verified restore, against a
baseline established *after* the fixtures were fixed to green. The results are
worth more than the count, because **two of the six are findings about the
tests**:

| row | verdict | what it means |
|---|---|---|
| **M2** | **BIT**, and **the pair separated** | the outside-drain test went red, the inside-drain test stayed green — the tests are isolating the property, not merely failing |
| **M4** | **BIT** on both named tests | plus three handler tests beyond its two named, noted rather than absorbed |
| **M6** | **BIT** with a clean control | the first attempt left a stray brace — the *harness's* encoding error, not the row's — and was corrected and re-run |
| **M1** | **DID NOT BITE — zero delta** | the same three tests failed with the same messages before and after the edit |
| **M3** | **the named tests did not move** | the property is actually pinned by a *different* binary |
| **M2 vs M4** | **perfectly orthogonal** | the guard tests stay green under M4 and the acquisition tests stay green under M2 |

**M1 is the most valuable row in the set, and it is a row that failed.** Its
named test passes with the guard deleted, so **the test does not test the
property** — the row stays open, and no rewording until it bites.

**And M3 is the other one: its named tests live in the wrong binary.** The row
named a test in one binary and the property is actually pinned by another. That
is not a broken row, it is a row that found where the coverage really is — and
**a row whose named test does not move is telling you where the assertion
lives, not that the code is safe.**

The general form, and it is the reason both directions are mandatory:

> **A row that bites proves a test is sensitive. A row that does not bite proves
> where the sensitivity is not — and the second answer is the one that changes
> what you write next.**

## A shared-contract field the FIXTURE stopped sending

The four keeper failures shared one refusal — *"journaled keeper update is
malformed"* — and the cause was the fixture, not the product: **it still sent
`bun_abi`**, a field the admission contract deliberately omits. **The fixture is
the stale side of a decision the code had already made correctly.**

That is worth stating precisely because it is the *third* time in this port that
a v2 field's removal was correct and something else had not caught up. **A
divergence is not always a defect in the port** — sometimes it is a caller that
never learned the field went away, and the test fixture is the caller that
finds it first.

## A mutation row run against a RED baseline is uninterpretable

The keeper rows were run, and the results were discarded and re-run — because
the baseline the rows were measured against was **not green**. Four tests in the
binaries under mutation were already failing for an unrelated reason.

**With a red baseline, a row's outcome is ambiguous in every direction:**

- the named test failed — but it was already failing, so the mutation proved
  nothing;
- the named test passed — but so did it before, so the row is unproven;
- a neighbouring test moved — and there is no way to attribute it.

**So the precondition is a green baseline for exactly the binaries the rows
name, established and recorded before the first mutation** — the same
"establish the tree's state before the measurement, not after" rule, applied to
the instrument rather than to the subject.

**And this is the same class as the mutation window itself:** a measurement taken
over a tree that is known to be in a transient state is a measurement of the
transient. The window announcement stops siblings *misreading* the transient;
the green baseline stops the *experiment itself* from being run on one. **The
first is hygiene for other agents, the second is a precondition for the result
to mean anything, and the second is the one that costs you the whole run.**

## A first fix that does not move the number is a FORK, not a verdict

`bootstrap_single_use` was **3 passed / 5 failed** after a real defect was
fixed. A real defect: **one unclosed parenthesis in `claim_bootstrap_token`'s SQL
— every bootstrap token redemption was failing.** The fix was correct and it moved
nothing, because there were **two** defects in one statement and the first hid
the second: `RETURNING bt.account_id` names the alias in a clause where SQLite
requires the bare column, failing with `no such column: bt.account_id`.

**8 passed / 0 failed** once both were fixed.

> **A first fix that does not move the number is a fork, not a verdict — it names
> the two places the cause could be, and picking one without evidence is how a
> real fix gets reported as your fix.**

The earlier formulation was *"a fix that does not move its predicted number is
either incomplete or was never the cause"*, which is a diagnosis. **The
operational form is a fork**: there are now two live hypotheses, the statement
and the path around it, and the next move is to distinguish them rather than to
declare the first one sufficient. **"I fixed a real bug" and "I fixed your bug"
are different claims, and a partially-green result is exactly where they come
apart.**

**And the corollary that makes this expensive: those two tests were never a race
test.** They assert on `accepted` and reported `left 0, right 1` — *both*
redemptions refused — while the other three said in prose *"the first
redemption: Internal"*. The first redemption failed, so there was no concurrency
to investigate. **They have been counted as atomicity evidence in every summary
so far and they are not.** A test failing for a reason other than its stated
reason is worse than one that has never run, because it is counted.

## Two references disagreeing is the evidence: find a third

The `RETURNING` failure was explained three ways in turn, and **the wrong ones
were falsified by running minimal cases rather than by reasoning:**

1. *"the alias is not visible inside a subquery"* — **false**; a minimal
   `UPDATE t AS bt … WHERE EXISTS (SELECT 1 … o.fk = bt.account_id)` succeeds.
2. *"the whole statement is malformed"* — **false** on the real schema; the
   exact extracted statement runs against all 27 tables, and the only failure is
   `RETURNING`. The system `sqlite3` here is **3.34.1 and cannot parse `RETURNING`
   at all** — it landed in 3.35 — so that was a version artefact.
3. *"the error is in the RETURNING clause"* — **confirmed**.

> **Two references disagreeing with each other is not a problem to resolve by
> picking one. It is a third reference point you have not looked for yet.**

The system `sqlite3` and the bundled one disagreed, **and the disagreement was
the evidence** — because a version artefact and a real defect produce the same
diagnostic until something independent breaks the tie. This is the same shape as
the `rtx`-vs-mobilecheck-vs-bundled divergence earlier in this port, and the rule
is the same: **when two sources of truth about an environment disagree, the
answer is a third independent source, not a tiebreak between the two.**

## The run that stopped early is a claim about the run, not the workspace

`cargo clippy` reported two diagnostics, both unused imports in `roost-keeper`,
and the integrator read that as *roost-coord is clean*.

**It was not. The run stopped at the first failing crate, and `roost-keeper`
sorts before `roost-coord`.** So *"only two lints remain in the workspace"* was
true of what the compiler reached and false of the workspace.

**This is the sixth instance of the `--keep-going` family, and it is the one
place nobody expected it: `clippy` is not a test run, and the habit of reading a
lint count as a verdict is as strong there as anywhere.** The family has now
produced: a `13 errors` under-read, a `27 errors` under-read, six missed binaries
from a truncated printout, a `97` from a `grep` that matched its own summary, a
stale `581/38`, and **a clippy count that described the crates alphabetically
before `roost-coord`.**

**And the count went DOWN — from an inherited 8 to 7 — by discovering the run had
stopped early rather than by fixing anything.** That is the useful shape of it:
a better number arrived from a better question, not from more work.

**The catch is the part worth naming: it went looking for a third reference point
rather than because the number looked wrong.** A linter reporting two unused
imports is not a suspicious number — it is exactly what a nearly-clean tree
looks like. **The only thing that made it checkable was the rule that two
sources disagreeing means looking for a third**, and the same agent had written
it down an hour earlier.

## A lint that is right is not always a mechanical fix

Two of the seven remaining lints were deliberately not fixed, and the reasons
are the point:

- **`install_cloudflare_jwks` returns `Result<(), ()>`.** Clippy is right: `()`
  is not an error type, and a caller cannot tell *why* an install failed.
  **Replacing it means choosing an error type** — and the choice is a decision,
  because the install runs once at boot and a `OnceLock::set` failure means a
  *second* install, which is a programming fault rather than a runtime
  condition. The right error type has to say that.
- **`authority.rs`'s unneeded `Ok`/`?` pair.** Clippy is right that the pair is
  redundant, and **which of the two to remove is a statement about whether that
  function can fail.**

> **A linter that reports a redundancy is pointing at a decision, not
> necessarily at an edit.** `collapsible_if` is mechanical; `needless_question_mark`
> is a question about whether the function can fail, and the lint does not know
> which you meant.

**So the remaining five are listed with exact locations rather than fixed in a
hurry, so that nobody re-derives them** — and the two judgement calls are
recorded as decisions, not as omissions, because a lint left unfixed with no
stated reason is indistinguishable from a lint nobody ran.

## One missing paren in 151 literals, and 6 of the 7 are the idiom

`claim_bootstrap_token`'s SQL literal had **one unclosed parenthesis** — the
`AND (` opening the minter-authority group never closed. SQLite reached
`RETURNING` at depth 1 and refused with `near "RETURNING": syntax error`, and the
RPC reported `Internal`.

**Every bootstrap token redemption was failing**, and no compiler can see it,
nor can a reader of fourteen lines of backslash-continued SQL.

**The class sweep is the part that generalises: of 151 SQL string literals, 7 are
unbalanced — and 6 are the deliberate "the `)` arrives from a `QueryBuilder::push`"
idiom. Exactly one is real.** So the sweep did not find one bug in 151 places; it
found **a defect and a convention that look identical to a paren-counter**, and
the only way to tell them apart was to read each one. **The negative results are
what close a class**, and here the six idiom sites are what prove the seventh is
a defect rather than the same deliberate shape.

**And the fix did not move the number it was predicted to move** — still 3 passed
/ 5 failed, all five redemption tests. So the missing paren was real **and
incomplete**: either a second problem in the statement or a second statement on
the path.

> **A fix that does not move the number it was predicted to move is either
> incomplete or was never the cause — and the report must say which.**

That sentence is the whole discipline in one line. "I fixed a real bug" and "I
fixed your bug" are different claims, and a green-looking partial result is
exactly where they come apart.

## A test that passes half the time is worse than one that always fails

`the_registry_answers_in_the_order_it_declares` asserts an order over relay ids
that are **random v4 uuids from SQLite's `randomblob`**. It passed at the
previous gate, fails now, with nothing in its files changed.

**A test that passes sometimes is worse than one that always fails, because it
gets filed as flaky infrastructure — a non-deterministic pass is
indistinguishable from a non-deterministic machine.** An always-failing test is
diagnosed in a minute; a sometimes-passing one is argued about for a week.

And the sharp part: **the same suspicion was falsified elsewhere.** Random-uuid
ordering was suspected in `workspaces_tree` and *disproved* at `:323-332`, which
rewrites tied ids to fixed values. **The suspicion was wrong in one place and
right in another, and only running it says which** — which is why a hunch has to
travel as a hunch, per site, and never as a pattern.

## A diff cannot distinguish "found nothing" from "found something and named it"

An agent working under a brief that forbids it from editing `src/` produces a
diff in which **a product defect is indistinguishable from no work at all.** I
read a clean-looking diff, concluded "this wave found no product defect", and was
wrong — the wave found the paren defect, named the file, and stopped at the
boundary, exactly as briefed.

**So: "the agent changed nothing" and "the agent found the cause and named it"
are identical in a diff, and only the report distinguishes them.** The lesson is
the mirror of the one above: **a diff is evidence about files, never about
findings**, and a conclusion drawn from an unchanged file list is a claim about
files being mistaken for a claim about the work.

## A stale number in a COMMIT BODY is worse than one in a chat message

A commit landed claiming **581 passed / 38 failed across 15 failing binaries.**
Two independent runs of the committed tree, from clean, both give:

```
584 passed / 35 failed / 3 ignored, 14 failing binaries
```

**Reproduced twice, identical — so the committed figure is stale, not flaky.**
It was measured before the last edits landed, and the commit body — the durable
artefact every future reader consults — states it as the tree's result.

**The rule sharpens from "do not quote a stale number" to: the commit body's
numbers are part of the commit, and a number measured before the final edit is a
false claim about committed code.** The stale-figure family has now produced:
a `13 errors` under-read, a `27 errors` under-read, a `97 binaries` off-by-one
from a `grep` that also matched the summary, six missed binaries from a
truncated printout, and now this — and **every one of them was correct when
measured and wrong when published.**

**The guard is cheap and it is the same one that caught the `97`:** a figure
quoted in a commit body is measured on the tree at that commit, and if anything
was edited afterwards the number does not ship. **Two runs agreeing is what makes
the corrected number trustworthy in its turn** — a single run of a tree that has
just been edited is the same measurement error wearing a different hat.

**The cleanest statement of the whole family, and the one to keep:**

> **A number is a claim about a tree, not about a run — and the two come apart
> silently.**

Every instance was *correct about the thing it measured* and *wrong about the
tree it was describing*. A `grep` that also matches the summary line is correct
about how many lines matched. A suite run that finished before the last edit is
correct about the suite. **The run is never wrong; the claim is.** That is why
none of these were caught by re-running — re-running reproduces the run, and the
run was never the problem.

**So the guard is structural, not diligence: a published figure ships with the
runs that agree, or it does not ship.** Not "check the number", but "the number
cannot be written down without the evidence beside it" — which is why the fix is
a file's byte count and two independent line counts sitting next to every
figure, rather than a reminder to be careful. **A rule that requires remembering
something is not a gate; a rule that makes the thing impossible to write alone
is.**

**And the check earns its cost only once someone declines to argue with it.** The
integrator had a number, I had two agreeing runs, and it took the reading over
its own — which is the only version of this that works. **A verification step
whose result gets negotiated is not a verification step.**

**So the general form of the trap: a counting command whose pattern also matches
the report about the count.** `grep -c "test result"` is one line longer than the
file it describes. The cheap guard is to print two independent counts beside each
other and require them to agree — because a single count has no way to be wrong
visibly.

## "They share a module" is now wrong four times out of six

| cluster | shared | one cause? |
|---|---|---|
| `agent_fixture` | 5 binaries | **yes** — one 32-hex constant, and two product bugs under it |
| `pairing_support` | 2 binaries | **yes** — `i64` against a `TEXT` column, and two more under it |
| `terminal_view_support` | 3 binaries | **no** — four subjects, four mechanisms, 24 tests pass |
| `tasks_support` | 2 binaries | **no** — `tasks_refusals` is 8/0 |
| `mcp_relays_support` | 4 binaries | **no** — three of four fully green |
| `keeper_update_support` | 3 binaries | **no** |

**Two of six, and both of those turned out to be product defects rather than
fixture defects.** So the guess is wrong four times in six *and* the times it is
right are the ones that matter most — which is the worst possible ratio, because
it makes the guess feel reliable right up until it costs an hour.

**Which is why a triage wave must not route by sharing.** Read the shared module
first because a fixture cause is *possible* there, not because it is *likely* —
and then be ready for the negative. **Four unrelated assertions in a working
module is a completely ordinary result**, and reporting it as such is a finding,
not a failure to find something.

**And the one that generalises past fixtures: a test whose expectation
contradicts its own input builder, ten lines apart, is a missing test rather than
a weak one.** `assert name == "ws"` sitting above a `create` helper that builds
`format!("ws-{folder}")` is not a weak assertion; it is an assertion that could
never have held, and it is provable from the test's own file without running it.

## A bounded read presented as a total is the `--keep-going` error again

A published per-binary breakdown **missed six binaries** — three
`workspaces_*` tests, all failing, all in the run — because the printout was
truncated at 130 lines and the tail was reported without checking what the cut
had removed. The conclusion survived (they are individual assertions, not
fixture cases) but **a number that was published was an under-read**, and this
is the same family as measuring without `--keep-going`: a limit in the tool
silently became a limit in the claim.

**So the rule generalises past cargo: when a result is bounded — by `head`, by
a terminal, by a page — the number is a lower bound until someone has read the
part that was cut off.** Truncation is not a display concern; it is a claim
about what was examined, and the claim is what gets cited later.

## A fixture can violate TWO rules and only ever show you the first

A test's configuration was refused. It violated **two** validation rules, and
they are checked in order:

- a team domain must be exactly one lowercase label under `.cloudflareaccess.com`
  — the test's literal was `team.example`;
- **behind it**, an audience must be exactly 64 lowercase hex characters — and
  *both* test files' audience was the 14-character `roost-coord-aud`.

**Any fix that corrected only the domain would have failed on the next line,
with a different message.** So the fixture costs a cycle no matter how the first
fix goes, and **the panic message never names the rule that was actually
violated** — it names the first one checked.

**So the rule: when a validation failure is fixed, read the validator's ORDER,
not just its first message.** A validator that checks A then B is a fixture that
violating either one produces the same-looking failure twice, and the second
attempt looks like the first fix not having worked. The crate's own unit test
held the good values, which is the fastest reference and the one a fixer skips
because the test file is what is already wrong.

**Same species as the `i64`-for-`TEXT` finding one layer out: a value that must
satisfy a constraint the error does not state.**

## Two tests sharing a process-wide counter produce a random result, not a flake

Two async tests in one binary shared **one process-wide lookup counter**, and the
harness runs tests concurrently — so each could read the other's eight lookups.

**That is not a defect in the product; it is a test whose result would have been
random.** And a random result is worse than a flake: once it surfaces it is
reported as *flaky* and filed as an infrastructure problem, because a
non-deterministic pass looks exactly like a non-deterministic infrastructure.

Fixed by counting **per `kid`**, with each test using its own. **The general form:
a process-wide counter shared by concurrently-run tests is a shared mutable
global with a test-shaped costume, and the fix is to key the count to whatever
the assertion is actually about** — not to remove the counter.

## A dependency's behaviour change is a class, and a sweep's NEGATIVES close it

`sqlx` 0.9 changed `Separated::push_bind` to **emit the separator first**. The
code did `separated.push(column).push(" = ").push_bind(value)`, where `push(" =
")` had already armed the separator — so it rendered `SET name,  = ?`.

**Every `update_workspace` was a syntax error.** It surfaced as
`WorkspaceError::Sqlite` and therefore as `Internal` at the RPC boundary — so
**the version guard never ran, and a stale write reported an internal fault
instead of `FailedPrecondition`.** Five of six failures in that cluster were this
one cause.

**The class was closed by a crate-wide sweep for every spelling of the idiom —
`.separated(`, `push_values`, `push_tuples`, `Separated`, `push_unseparated`,
`push_bind_unseparated` — and `workspaces.rs:192` was the only site chaining
`push` and `push_bind` on the same `Separated`.** The other six sites are all
one-element-per-`push_bind`, the idiom insensitive to the change, and correct
under both versions.

**So: the negative results are what close a class, and the positive one is what
opens it.** Six sites enumerated and dismissed is the evidence; one site named is
a fix. A sweep that reports only the hit has told you nothing about the other
five.

**And the fix moved the predicate onto the `QueryBuilder`, because a `Separated`
inserts commas** — which is the general shape: when the container's semantics are
the bug, the fix is not a different call order within the container, it is a
different container.

## "Lossy, not wrong" is a third category

A caller sending `if_version > i64::MAX` gets `VersionMismatch` rather than a
rejected value, because the request is clamped to `i64::MAX` and matches no row.
**That is lossy, not wrong** — the request is refused, the database is not
corrupted, and no test sends a value that large.

**This programme keeps inventing binaries for *correct* and *broken*, and a
defect report that only has those two slots will push a lossy behaviour into one
of them by default.** So the third category is named explicitly: it goes in as an
**open decision with its reasoning, and it must not acquire a guard by
accident** — because adding a check nobody decided on is how a documented
limitation becomes an undocumented behaviour change.

## THE GUARD-ESCAPE CLASS IS CLOSED

Open since before this wave: **the only test in the programme that can see a
lock held across an `await` had never been executed.** The verdict, in the three
terms fixed in advance, and how each resolved:

- **HANG — NO, and that was the load-bearing observation.** All eight concurrent
  callers completed; each crossed the real `yield_now().await` inside
  `CloudflareJwks::jwk` and each received a verified identity. **If a lock were
  held across the await, the second of eight callers would block and the test
  would never have finished.** It finished in 0.11s.
- **FAIL inside the concurrency body — YES, once**, at *"every caller really did
  reach the key ring"*, `left: 16, right: 8`.
- **PASS — no, on that run.** The test failed on its own arithmetic: the
  fixture's counter is `+= 2` per call while the assertion expected one per
  caller. Eight callers × 2 is 16, exactly. **The instrument and the expectation
  disagreed with each other, not with the product.**

**After naming the constant, the test passes: 5 passed / 0 failed.** So the
class is closed on the no-hang evidence *and* on a green run, and the fix between
them was the test's own arithmetic.

**The route took three failed runs, and none of the first two was the answer.**
The first panicked in the test's own `config()` on a team domain; the second was
behind that on an audience length; only the third reached the concurrency body.
**A test that cannot construct its own inputs will tell you about its inputs for
as long as you let it, and never about the thing it was written to check** —
which is the same shape as the vacuous-fixture class, arriving from the
opposite direction.

**And the fix is a general rule, because the failure is general:** a counter that
moves by N per call must be asserted as `CALLERS * LOOKUPS_PER_CALL` in terms of
a **named constant**, never as a bare number that happens to be right for one of
them. Here the bare `8` was wrong by exactly the per-call factor, and it had
been sitting in a test nobody had run.

**A residual race the per-`kid` fix did not close, recorded rather than
silently accepted:** the ring is a process-wide `OnceLock` and three other tests
in the same binary also use `KEY_ID`, so the `before`/`after` delta isolates them
only if none increments *during* this test. **Keying a counter to what the
assertion is about is necessary and not sufficient** — a counter shared across
concurrently-run tests needs the tests serialised, or the counter per-test
rather than per-subject.

## The input that does not say what the test's own name says

This has now appeared **four times independently, in three unrelated areas**,
which makes it a class rather than a coincidence:

- a test named `a_wrong_requester_token_finds_nothing` called a helper that
  **hardcoded the correct token**, so "wrong" was a *successful* confirmation;
- `workspaces_sync_delta` asserted `created.name == "ws"` ten lines above a
  builder that produces `format!("ws-{folder}")` — provable without running it;
- a trust-boundary test passed `UNADMITTED` (documented as a SESSION uuid) as a
  **view id**, so validation passed, the declaration was admitted, and **the
  `!allowed` arm was never reached — the test was exercising nothing**;
- a "stranger" identity built from the same `WORKER_FP` the harness registers
  the owner with, so the stranger **was** the owner.

**The general form: a test's input builder and its name are written separately,
and nothing checks that the builder produces what the name claims.** The name is
the specification; the builder is the fixture; a test is honest only when the two
agree, and **the assertion cannot detect the disagreement** — it either passes
vacuously or fails for a reason the name does not describe.

**So the check is mechanical and belongs before the run: read the test's input
builder and compare it to the test's name and doc comment.** A name naming a
*wrong*, *stranger*, *expired*, *revoked* or *foreign* value is a claim that the
fixture is distinguishing it, and a builder that cannot distinguish it is a
vacuous test. **This is the last thing a compiler, a linter, and a reviewer
skimming the assertions will ever catch — it is only visible in the gap between
the signature and the setup.**

And the corollary that makes it a real risk rather than a style note: **four of
these were found by reading inputs, not assertions, and one of the four was
hiding a genuine product defect behind it** — a same-revision ACTIVE declaration
could revive a released claim, because the admission guard was unreachable for
release-created claims. **A vacuous test is not merely wasted; it is a place
where a real defect goes to hide.**

## A test that drops the handle asserts the opposite of the contract

Three separate tests in one wave failed for the same reason, and it is a class
with a name: **an RAII handle was discarded, and the test then asserted the
resource was still held.**

- a `subscribe()` helper calls `bus.subscribe(..)` and **discards the returned
  `Subscription`.** Its `Drop` removes the listener, so the sink records nothing
  — observed as `closing.len() == 0`. The assertions were correct; the test was
  observing a torn-down subscription.
- a capacity test loops `registry.register(request(i)).is_ok()` and **discards
  the `AgentStatusWaiter`.** Its `Drop` deregisters immediately, so `total` never
  accumulates and the global bound is never reached. **The test's own
  bookkeeping was destroyed by RAII before the next assertion.**
- two more tests fill a per-session bound in a loop that discards each waiter, so
  every slot is released before the bound is probed and **neither refusal ever
  occurs.**

**A dropped binding is valid syntax, so every resolver-based check is blind to
this by construction — and here the handle is not even a binding, it is a
discarded `Ok` value nobody is required to use.** The compiler has nothing to say
about a handle you chose not to keep.

**The statement to carry: a test that drops the handle asserts the opposite of
the contract.** That is sharper than "the test is wrong", because it explains
*why*: the resource's whole lifetime is managed by the handle, so releasing the
handle is releasing the thing under test. And the second one is a *consequence* of
a semantics fix — once a pending waiter was made to actually hold its slot, a
test that dropped it was asserting the old, broken behaviour.

**So when a test fails with "the thing is not there", check whether the test is
holding the thing.** The fix is a `Vec` of handles kept alive to the assertion,
not a corrected expectation.

**And the rule was then made falsifiable rather than merely plausible.** Every
test in the two affected files was classified by whether it *holds* or *drops*
its waiters, and compared against the measured result:

- **7 tests hold their waiters — all 7 pass.**
- **3 tests drop them — all 3 fail**, and they are exactly the three named.

**Zero exceptions in either direction. A perfect predictor across 10 tests is not
something a coincidence produces**, and that is the difference between a rule
that fits and a rule that is confirmed. A rule with no counterexample checked is
a pattern; a rule with all 10 cases on the predicted side is a mechanism.

**The counter-example is what makes it correct rather than nearly-correct.**
`a_released_wait_is_not_leaked_when_the_subscriber_is_gone` **drops its waiters
deliberately and asserts the slot IS released** — which is correct. So the
refined rule is not *"never drop"*:

> **What matters is whether the drop is deliberate, and whether the assertion is
> about the state after the drop.** A handle dropped to make a resource go away,
> followed by an assertion that it went away, is the test working. A handle
> dropped because the return value was unused, followed by an assertion that it
> is still there, is the test asserting the opposite of its own intent.

That counterexample is also the thing that keeps the rule from being
over-applied into "never discard a value", which would be wrong and would
generate false findings.

## A test may be the thing that is out of step with the port

One assertion was judged **wrong** and deliberately not edited: a push-scheduler
test expected a notification for an `idle -> blocked` transition, and
`classify_transition` awards `Blocked` only for `working -> blocked`. **The port
matches v2 exactly** (`agent-status-push-scheduler.ts:98-104`) — so the test is
out of step with the code, not the reverse.

**An idle→blocked transition producing no push is defensible** — an idle agent is
not mid-turn, and a blocked-notification requires a turn to be blocked in — **but
the test was asserting a product decision nobody has made.** That is routed as a
human call, not a mechanical fix, and it is recorded rather than edited: **the
right move when the code and its reference agree and the test disagrees is to
stop and ask which of the two is the requirement.**

And a near-vacuous sibling was flagged rather than counted: one test builds a
`PushTransitions` twice and asserts `is_enabled()` true and false — **a
constructor predicate, not the allowlist's effect** — while its sibling does
exercise the real path. Flagged so nobody counts it as allowlist coverage.

## A fixture can hold a property that does not survive a merge

`tests/auth_device_support/` exists for one reason: `Scratch::second_core()`
opens a **second `CoordDb` on the same database file.** That is what makes the
redemption race a real race instead of a pool-of-one serialisation, and it is
why `two_simultaneous_browser_redemptions_leave_exactly_one_principal` is not
vacuous — a read-then-write claim would pass it if both redemptions shared one
connection.

**If that fixture is folded into a shared one and `second_core` is lost in the
merge, both race tests keep compiling, keep passing, and stop testing the
property.** Nothing fails. That is the worst artefact in this programme: a green
test that is now a tautology, produced not by a bad edit but by a *good* one.

**So a fixture refactor has to be read as a change to every test that consumes
it, not as a tidiness change** — and the property to look for is not "does it
still compile" but **"what made this test able to fail at all, and is that still
here?"** The rest of that fixture (a scratch dir, a booted core, an enrol
helper) is ordinary and is what would be worth sharing.

## "Never observed" is a fourth category, and it is not "failing"

22 of one slice's tests **have never been observed in any state.** They are not
"still broken" — they are an **absence**, and an absence contributes no signal to
a second-cause analysis because there is no before.

**So a failure map has four categories, not three:** 0-pass-fixture, failing,
partially-green, and **never-observed.** The first three all have a before. The
fourth has none, and the question for it is different in kind: **not "what is
still red" but "what does the first successful compile surface?"** Those are
different measurements, and counting a never-observed binary as a regression — or
as a pass — is a category error.

This is why a wave's first run of a new target is a *discovery* step and not a
*measurement* step, and why "the suite ran for the first time in this crate's
history" is a fact with no baseline attached.

## The count is not the deliverable, the shape is

The full accounting of 100 failures, when sorted, has **exactly three shapes**:

| shape | count | what it wants |
|---|---|---|
| two shared fixtures, 0-pass | **47** | read one file, find one cause |
| partially-green binaries with a known module | ~24 | individual assertions |
| individual assertions in otherwise-green binaries | ~29 | individual attention |

**"A binary that moved less than its shape promised has told you something."**
The failure mode is an agent reporting a clean number while a binary that should
have moved 9/9 moved 4/9 — and the count conceals exactly the information that
matters, because 4/9 still *looks* like progress.

So the deliverable of a fix is a **shape**, and the diagnostic is a
**discrepancy between the count and the shape that count was supposed to have**:
- a 0-pass binary that still has failures is a *second cause underneath*;
- a partially-green binary that moved nothing is a *cause nobody has looked at*;
- a number in the wrong group that changed at all is *drift*, and drift is a
  finding, not noise.

**47 + 53 = 100, and the three groups are disjoint by construction** — which is
what makes the wave routable at all. A failure count sorted by owner would have
overlapped: one slice's two binaries sit in two different groups, because one
declares a shared fixture and the other declares none and still fails 11 of 12.

## Before fanning out over a shared fixture, count its consumers exactly

Two agents were about to be pointed at `tests/agent_fixture/` and
`tests/pairing_support/`, and the convergence risk was real: three agents
editing one fixture with no owner. It was closed by counting, not by grepping a
sample — **`agent_fixture` has exactly five consumers and `pairing_support`
exactly two, with no `#[path]` indirection on either**, so the search is complete
rather than a sample.

**And the count buys a second fact that decides the sequencing: neither fixture
is load-bearing for anything currently green.** If it were, fixing it would break
a passing test and the agent would have to reason about a regression it did not
cause. **A shared fixture whose every consumer is already failing is the safest
thing in the tree to hand to an agent** — the blast radius is the set of things
that are already broken.

## A fixture fix is not done when the count rises

The number of passing tests is a **proxy** for the cause being gone, and the
second cause under a fixture is the one that gets read as *"still broken"* and
attributed to the agent who just fixed the first.

**A fixture killing 35 tests can easily have been hiding three more, and an agent
told to reach 35/35 who stops there has not finished.** The completion criterion
is the cause, not the count: after the fix, the re-measured map must be **read**,
and any binary that moved less than expected is a *finding about the second
cause*, not a rounding error.

**This is the same rule as "establish the tree's state before the measurement,
not after"** — a count taken across a fix is two measurements, and reading it as
one is how a wave reports progress it did not make.

## The audit that cannot see the defect, and the noise you must discard

A slice deleting a dead `FixedEntropy` block used a doc comment further down
as its end boundary, **and the four prime constants sat between the block and
the marker.** It deleted a larger slice than it had read. It then verified the
names it *expected* to disappear were gone — **and did not check what had gone
with them.**

**The recovery is the reusable part.** It re-derived the constants **out of the
file**, not from memory, and checked each is byte-identical to the integer it
had generated. That last check matters because each 309-digit constant is
wrapped across four lines with `\` continuations: **a transcription slip would
still have parsed as a prime**, and would have made
`a_signature_from_another_key_does_not_verify` assert that a key differs from a
key it is identical to. The mutation would have compiled, run, and passed —
**a green test asserting the opposite of its name.**

**Then it established the limit of its own instruments, which is the finding:**

> A dropped binding is **valid syntax**, so `rustfmt --emit stdout` is blind to
> it by construction — and so are cross-module import resolution and dangling
> `pub mod` detection, because all three are about *declaration and
> resolution*, not about *use*. A third sweep was written, found to be pure
> noise (it matches prose in doc comments, SQL inside string literals, and
> methods on imported types), and **discarded rather than reported.**

**No static audit in that set can catch this class. What catches it is reading
what a deletion removed — a review step, not a script.** And the discipline is
the same one as a signature change: **look at the other side of the edit, not
only the side you meant to touch.**

Two rules, both cheap:

- **A sweep that is mostly noise is not a finding — it is a false assurance.**
  Discarding it is the correct result and belongs in the report, because "I
  looked for this class and cannot see it statically" is information a gate can
  act on, and "here is a check that fires on 400 doc comments" is not.
- **For a test whose fixture is a large constant, the fixture must be
  byte-verified against its source of truth.** A constant that is 98% digits and
  2% transcription is a prime that is not the one you meant.

## Making the table agree with the gate by recording LESS

The integrator narrowed `MiscDbExportUrl` from `DeviceOnHost` to `Device` after
finding that the gate enforces no locality. That made the table *consistent with
the code* — and it was **a parity narrowing, which is the same defect as a row
that over-claims, pointing the other way.** v2 is `requireAccountDevice` **AND**
an unguarded `assertOnHost`; a device *and* on-host is precisely what
`DeviceOnHost` means. Recording `Device` because the gate cannot tell them apart
discards a real requirement so that a reader's check passes.

**The two correct sentences are different and only one of them is useful:**
"the variant is meaningless" and "the requirement is real in v2 and the gate does
not enforce it." The first invites deletion; the second is what a reader needs.

And v2's five `assertOnHost` sites are **two shapes, not one**:

- **unguarded `assertOnHost(...)`** — a credentialed device *and* on-host. One
  site. This is `DeviceOnHost`.
- **`if (!caller) assertOnHost(...)`** — on-host as an **uncredentialed
  fallback**, admitting a caller holding no key at all. Four sites: pairing ×3
  and the devices revoke.

`principal_satisfies` can express neither: the first is an extra restriction on
a caller it has already admitted, and the second admits a caller with no
credential, which the gate only builds for a request that carried one.
**Flattening two shapes into one lost the fallback reading that four rows depend
on** — the same flattening risk as a shared-crate parallel implementation, one
level up, in a *record* rather than in code.

**A parity question belongs in a commit body as a named decision, not in a diff
at an integration gate** — and a third sentence was available and unsupported:
"`Device` is correct because on-host is out of scope for the port." Only
"`Device` is correct because v2 asserts no locality here" is a claim the
evidence supports, and choosing between them is the whole job.

## A handoff that needs two actors is not a request for one of them to go first

A slice named a file in its report as something it would do *once the
integrator placed the `mod` line* — because `auth/mod.rs` is integrator-owned.
The integrator placed the line immediately. **`pub mod` naming a file that does
not exist is a hard error for the whole crate**, so the coordinator was red for
a while.

The slice was right that `mod` placement is not its to decide, and the
integrator was right that the declaration is not its to author, and **the design
was wrong in both cases**: the request carried a build-breaking window inside
it, and neither party could see the window from where they stood.

**The rule: a handoff that needs two actors must not be phrased as a request
for one of them to go first.** Either write the file and *then* ask for the
declaration, or ask for the declaration and accept that the build stays red
until the file lands — but never "do this once I do that", because that
serialises two actions across a message boundary and leaves a window neither
actor is watching.

The same slice had already done it correctly three times in one report:
`cf_access_keyring`, `db_statements` and `rpc_bootstrap` were all file-first,
then asked. **It named the difference itself: it chose per-conversation instead
of per-rule.** That is the shape to watch for — a convention that holds for most
of the work and fails on the one case that crosses an ownership boundary.

And the practical guard: a `mod` line whose file is not on disk is a build
failure with no useful diagnostic, so **place declarations as the last step of
a change, never as a response to a request.**

## The test that was asserting the lie

`method_route_coverage.rs` carried
`the_on_host_gate_is_exactly_the_export_url_and_nothing_else`, asserting that the
`DeviceOnHost` set was exactly `{MiscDbExportUrl, WorkersPrepareKeeperUpdate}`,
with the comment *"host-local changes a remote device has no business
authorising"*.

**It was green precisely because it agreed with the table, and it never asked
the gate.** The test restated the claim under test instead of checking it, so a
variant that nothing enforced was documented by a test that enforced nothing
either. Two rows claimed a locality the auth gate does not implement —
`principal_satisfies` answers `is_browser` for `Device` and `DeviceOnHost`
alike.

It is replaced by `no_row_claims_a_locality_the_auth_gate_does_not_enforce`,
which asserts the **empty** set and then names where each method's locality
really lives: the export is checked at `http/listener.rs:248`, and the keeper
update has no locality check in v2 either.

**The general form, and it is the worst instance of the verification ceiling in
this programme: a test that restates the claim under test is worse than no
test.** It occupies the slot where a real check would go, and it is *more*
believable than an absence because it has assertions in it. The question to ask
of any coverage contract is **"what would this test print if the thing it names
stopped being true?"** — and this one would print the same thing.

Note also what was done with the now-unused variant: `DeviceOnHost` is kept,
with a doc saying it cannot be enforced and why, because expressing it needs an
on-host caller with no credential and the gate only builds a `Caller` for a
request that carried one. Deciding whether to build that caller is a **parity**
question — dropping v2's credential-less device-recovery path — and it belongs
in a commit body as a named decision, not in a diff at an integration gate.

## An auth level that is recorded but not enforced

`DevicesRevoke` is `DeviceOnHost` in v2 and in the port — an on-host caller with
**no credential at all** may revoke a device, because the operator who has lost
their only device is exactly who needs that path. The row records
`DeviceOnHost` correctly.

**`principal_satisfies` treats `DeviceOnHost` identically to `Device`: it checks
the principal and not the locality.** So the recorded level is a claim the
enforcement does not make, which is the specific failure the route-row contract
exists to prevent — a security document that reads correctly and describes code
that does something narrower. The handler implements v2's rule anyway, so it is
correct the moment a variant expresses it, and the credential-less path is
currently unreachable in Shape A because every handler takes a `&Caller` and the
gate only inserts one for a request that carried a credential.

**The general form, and it is the one to check at every row flip: a row that
records a level nothing enforces is worse than a row that records `Device`,
because it buys the reviewer the assurance the code does not have.** Ask what
distinguishes the variants at the point of the check, not at the point of the
declaration.

## Two majors of one crate in one build

The workspace pins `sha2 = "0.11.0"`. `rsa 0.9.10` depends on `sha2 0.10.6`
and re-exports it as `rsa::sha2`. `VerifyingKey::<D>` is generic over
`Digest + AssociatedOid` **from digest 0.10**, so the workspace's `Sha256` is
rejected by a bound rather than by a name.

**This is invisible until the bound fires, and it fires in the CALLER, not in the
dependency declaration** — so the error names a type the reader believes is the
one they imported. The keyring uses `rsa::sha2::Sha256`, and the RSA test
signs with the same one, because **a test that signs with a different digest
than the code verifies with cannot pass for the right reason.** That is the
fixture rule again, one layer down: the fixture must compute its expectation the
way the *peer* does, and here the peer is a different major of the same crate.

**When a generic bound rejects a type from a workspace-pinned crate, check the
resolved version before you check your own code.** `cargo tree -i <crate>` is the
command, and the answer is frequently that the crate you imported and the crate
your dependency compiled against are not the same crate.

**The shape: `__buffa_unknown_fields`.** buffa generates that field on every
message, and a struct literal naming only the fields you can see is missing it.
It is invisible to every check that reads the `.proto` or the visible field
list, because the field is *added by the generator* — which is why a careful
hand-audit cannot find it and nine of fourteen literals were wrong.

**The fix is `..Default::default()`, not `__buffa_unknown_fields: None`.** The
generated types derive `Default`, and a literal that names the generated field
is an edit waiting for the next proto change — which then reads as a real
failure rather than as the generator having moved.

**And because it is mechanical, it should be a check rather than a habit:** a
two-line sweep for every `roost_proto::<Message> { … }` literal that does not
end in `..Default::default()` or a `MessageField` is exhaustive where a
hand-audit is not. That is the general form of a lesson worth keeping —
**when a class of mistake is findable by a pattern, write the pattern down as a
check instead of telling people to look harder.**

## Two rules from a racing pair that found each other's bugs

**An adjacent, in-scope, type-checking vocabulary is the most dangerous thing
on the shelf.** F6 was commissioned to stop `resize()` collapsing distinct
refusals into one I/O string — and the implementation written to stop it typed
the refusal with `PtyInRejectReason` (the **input** code set, five values, 1..5)
instead of `ResizeRejectReason` (nine codes). **Byte 4, `resize_error`, decodes
as `ChildExited`,** so a caller takes the input-recovery path for a resize that
failed as something else. It compiled. Three tests written against it by its
author would have passed.

**The check is not "does it type-check". It is: does this type's value set match
what the wire byte on this path actually carries** — and the reference is the
protocol's own table (`protocol-terminal.ts:19-48`, `:50-60`), never the nearest
enum in the crate. Two enums of the same shape with different numbers are
indistinguishable to the compiler and completely distinguishable to a user.

**A test whose expected value equals the requested value is not a test of a
query.** `terminal_state` resized to 132x43 and then asserting 132x43 asserts
what was asked for, so a client that echoes the request straight back passes it.
**The discriminating case is a stale sequence:** `apply_resize` returns the
applied sequence unchanged, so a lower-sequence request is acknowledged with the
*first* geometry — and an echoing client cannot produce that answer. If the
expected value could be produced by doing nothing, it is not a test.

**What caught it was a fresh reader asking what the wire carries** — the same
move as the read-only audit that found F5, and the reason the F5/F6 brief is
framed as a question about TypeScript callers rather than as a list of methods
to add. Two agents who raced, coordinated, and produced a better result than
either would have alone is worth more than a clean hand-off between agents that
never overlapped.

## Two more, from the last error standing and from a defect a sweep caught

**A correct definition does not imply a correct expression.** The final
compile error of the C1 wave was `E0609: no field expired_ids on type
CreateOutcome`, and the slice's showing was *true at every point*: the type was
defined once, the field was on all three variants, and there was one access
site. **None of that was the problem, because Rust does not let you name a
field on an enum at all.** `&creation.expired_ids` is not a field access that
happens to be missing; it is a field access *the language does not have* — only
a `match` knows which variant's field you meant.

So the failure is a different shape from every other error this wave: **the
API was correct and the form of the use was not**, which is why no amount of
re-reading the neighbour's definition could ever have found it. The same shape
as the parse error that reported "1 error left": **evidence that was true,
describing a different object than the one that was broken.** Both are worth
remembering as a pair — a true statement can be about the wrong thing, and the
fix is to ask what object the evidence is actually about.

**A constant that stops being referenced is a behaviour that stopped
happenin, and the unused-import warning is the only thing in the toolchain that
says so.** Replacing `buffer_unordered(MAX_CONCURRENT_SENDS)` with a
`FuturesUnordered` to clear an `FnOnce` error silently deleted the concurrency
ceiling: `FuturesUnordered` runs everything handed to it, and every subscription
was pushed up front. A dashboard with two thousand stale subscriptions would have
opened two thousand HTTPS round trips at once, which is exactly what the constant
exists to prevent and what v2's fixed worker pool holds. **The compiler could
not see it.** The only evidence in the entire build was that the import had
become unused.

This is a much better argument for treating warnings as gate failures than
"clippy is strict": a warning is frequently the sole signal that a *limit* is no
longer being applied, because a limit is the kind of thing that compiles
perfectly well without it. **The imports you stop using are the behaviour you
stopped having.**

## What lock-free verification cannot see, stated by the slice that ran it

Five errors in a coordinator test fixture, fixed at the end of a wave whose
entire method was lock-free checks. **They are three classes, and only two of
them are the class everybody expects.**

1. **A stale name** — a definition that moved and its import that did not. A
   compiler finds this by shape, and so does an import audit. Fix it by
   re-reading the current signature, **never by aliasing**: a second spelling
   of one function is the fork this port keeps paying for.
2. **An unbound variable** — a value used in a struct literal that was never
   bound anywhere in the function. A compiler finds this by shape; **no static
   audit finds it at all**, because there is no declaration to be missing. One
   of these had been in the file since it was written.
3. **A wrong receiver** — a fixture method called as a free function, with the
   import still present so the name resolves. **An import audit cannot see it,
   because the check it performs — does this name resolve? — passes.** Only a
   reader sees that the thing resolved to the wrong shape. Arity audits do not
   help either: they count arguments, not receivers.

**The number that matters: five audits over those nine files, every one reported
clean, on a tree containing an unbound variable.** That is the honest ceiling of
the method, and it is worth stating in a slice's own report rather than only in
an integrator's — "my checks cover syntax, binds and literals, and type
correctness is outside all three" was the sentence that generalised it.

**A fourth, smaller instance in the same file, and it is an attribute making a
claim the code does not support:** `#[tokio::test]` on a function with zero
`.await` and no I/O. The test is now `#[test]`. **Attaching a runtime to a test
that does no I/O is the same mistake as naming a field on an enum** — the
annotation asserts an asynchrony the code does not have, and a reader who
believes it will be confused by why the test is instant.

## The test that was testing an empty database, and the class nothing can see

Five `insert` calls in a fixture were missing `.await`. `insert` is async, so
each call built a future and dropped it: **the fixture seeded nothing, and
every assertion in that file was checking rows that were never there.** The
file compiled, passed review, and passed five of its author's own audits.

**This is worse than having no test**, because a missing test is visible in
review and a test that asserts against an empty fixture looks exactly like
coverage. It would have passed CI, and its failure mode is a sweep that never
ran being indistinguishable from a sweep that ran and reclaimed nothing.

**Nothing in the lock-free toolkit can see it.** Not the parse check — a dropped
future is valid syntax. Not the reference audit — the name resolves. Not the
arity audit — the arguments are right; the call is simply never made. The
diagnostic is about a value's *use*, not its shape, so it is the one class
where a compiler is not merely faster than a script but categorically
different. **"Compiling is not passing" is usually about assertions; here it was
about the fixture, and the assertions were fine.**

The corollary for a gate: **a test binary that has never been executed has an
unverified fixture.** Type-checking proves the file parses and the names
resolve; it proves nothing about whether the rows a test asserts on were ever
written. That is a distinct claim from "the tests pass", and a gate that
reports the first must not imply the second.

**And the neighbouring finding, which is about error messages rather than
tests.** A fully-qualified `account::LiveSelector::ById` produced
`E0603 private` — a *misleading* error, because `LiveSelector` had moved to
`rows` and the stale prefix made a public enum read as private. **An error that
names the wrong cause is worse than one that names nothing, because it sends
you to change a visibility that was never wrong.** The same shape as the enum
field access: the evidence is about a real thing, and the thing is not the
thing that is broken.

**"The binary compiles" is not "the fixture works", and the gap between them is
where a test becomes a lie.** Of eight test binaries in one slice, two were
clean only *after* the compile found real defects in them — and one of those was
a fixture that seeded nothing. So the compilation of a test binary is not
confirmation that its fixture works; **it is confirmation that its fixture
type-checks.** A test suite in that state is *nominally* sound and *unexecuted*,
which is a third claim, distinct from both "green" and "broken", and a gate
reporting it should say which of the three it has.

**And the row that did not bite, which is the sharpest instance of this rule in
the wave.** A mutation inverted F1's arrival-order drain — `next_event` popping
the *newest* deferred frame instead of the oldest — and **the entire keeper suite
passed**, including the test written to pin exactly that. One deferred frame is
indistinguishable under `pop_front` and `pop_back`, because a single element is
its own head, so the test could not have caught it at any strength.

**The fix is the same shape as every other discriminating test in this file: the
expected value must be one a wrong client cannot produce.** Two deferred frames,
asserted in order. One frame cannot distinguish; two can. And the property to
check when a test does not bite is not "is it running" first but "is it
discriminating" — a test can be present, collected, executed, and unable to fail.

## F1's proof, complete — and what it took to get there

Both halves of the keeper's PTY-loss fix are now pinned, **each with a control
that distinguishes "this property moved" from "something else broke":**

| Half | Mutation | Observed |
|---|---|---|
| **loss** — the output is handed over at all | `defer` set to drop | both F1 tests fail, `left: []` |
| **order** — the output is handed over in arrival order | `pop_back` instead of `pop_front` | the order test fails with `left: ["third","second","first"]` **while the loss test passes** |

**The control did its job on the run where it was needed.** Both going red would
have meant the patch was two mutations and the result would have said nothing
about ordering — which is exactly what happened on the first attempt at the
order mutation, and exactly what announcing the control is designed to catch.

**The durable form of the rule is not "report your own contamination" — it is
"establish the tree's state BEFORE the measurement, not after".** A lead filed a
green suite and noticed a sibling's open mutation only afterwards, and the
second run may have straddled the restore. Both numbers were discarded and the
sibling's `git status`-founded account was taken instead, because it quoted a
check and the other quoted a recollection.

Self-naming on the second pass is real and it is worth less than never filing
the number. **A measurement taken on a tree whose state was not established
first is not a contaminated measurement, it is an unattributed one** — and the
cheapest defence is a `git status` before the run, which costs a second and
discards nothing.

**And the order test was already discriminating, which I had wrong.** I said it
needed two deferred frames to have teeth, on the principle that one frame is
indistinguishable under `pop_front` and `pop_back`. The fixture sends **three**,
 so the principle is satisfied — the lead corrected my framing rather than
letting it be recorded as a gap that needed closing.

**The refutation that settled whether the deferral path is exercised at all is
the best argument in this file.** The hypothesis was that the fake wrote the
chunks and the ack in one burst and the chunks might never be deferred. The
decisive answer: **the three `PtyOut` frames and the `ResizeAck` go into one
ordered channel with the answer last, so `wait_for_reply` must consume all three
before it sees the ack — it is not a race, it is a single ordered channel with
the answer last.** And the observed vector settles it independently: under the
hypothesis the drain would read `["first","second","third"]` under *both*
`pop_front` and `pop_back`. **A reversed vector is only producible if the frames
went through the deferred queue** — you cannot get third-second-first out of a
channel that was never reversed.

**Which means the property is observable through this API, and a test that
cannot distinguish is a question about the test rather than about the design.**
That is worth stating because the opposite conclusion — "it is not observable
here" — would have been a reasonable guess and would have been wrong.

## The first green ported-crate suite, and what it does not prove

`cargo test -p roost-keeper --no-fail-fast` on the restored tree — **131 passed
/ 0 failed, 22 binaries, one uncontended run**, with the crate's `git status`
verified clean immediately before and after. **This is the keeper's number, not
the worker's**: the worker crate does not link, and a reader who sees 131 green
will otherwise assume the track is further along than it is.

**Why the ordering property is observable at all, which is the part worth
keeping.** The doubt was that whether `wait_for_reply` consumes the chunks
before the ack might be a race — and that if the reader won, the deferred
queue would be empty and `next_event` would read `events` in arrival order
regardless of the drain. It is not a race: the three `PtyOut` frames and the
`ResizeAck` enter **one** ordered channel with the answer last, so
`wait_for_reply` must consume all three whichever thread wins.

**And the observed vector settles it independently of the argument.** Under the
empty-queue hypothesis the drain comes from `events` in *arrival* order and
yields `["first","second","third"]` under `pop_front` **and** `pop_back`. You
cannot produce `["third","second","first"]` from a channel nobody reversed.

So the property is observable through this API and is pinned, rather than being
asserted by a test that could not have distinguished it. **The opposite
conclusion — "it is not observable here" — was a reasonable guess and would
have been wrong**, and the reason it was wrong is a structural property of the
channel rather than anything in the test.

**And the two rules are the same instrument, which took two agents and a
longer argument than it should have.** The F1 control — *the loss test must
still pass, so the mutation moved order and nothing else* — and the hygiene rule
— *establish the tree's state before the measurement, not after* — are one idea:
**a measurement is only as good as the state it was taken against.** The loss
test passing is what made a red order-test readable; a clean `git status` is
what made 131 quotable. In both cases the cheap control is the same move, and in
both cases its absence costs the whole result rather than degrading it.

**And the sharpest argument for the whole discipline, which is what a mutation
bought rather than what it proved.** A terminal-view slice removed an `unused
mut` by restructuring, and the restructure exposed a real defect: `drop_record`
removed a key from `socket.views` silently, so **a session that closed while
browsers had it open left every one of those sockets still watching it**, and the
Sync driver went on delivering cells for a session that no longer existed.

The defect was in code no test covered and no review had questioned, and it
surfaced because a lint's removal forced the shape to change.

**So a mutation's return is not only that a test bites. It is that changing one
line of a covered file re-shapes its neighbours, and the neighbours are where
the unexamined behaviour is.** A mutation that fails to compile is telling you
something about the shape; one that passes is telling you the shape held.

The framing that makes it stick: **reporting your own contamination is a
confession; a `git status` before the run is a control.** One is a statement
about the past, the other is an instrument for the future, and only the second
prevents the next occurrence.


## Sixty files that existed in one working tree

The worker's entire W-1 wave — **53 new files and 7 modified, every slice's
output** — sat uncommitted for hours. Only the module skeleton and the lead's own
seams had ever been committed. A lead came within one `git checkout` of losing
all of it, clearing a mutation it had itself applied; the only reason that was
survivable is that the file it would have restored happened to be backed up in
`/tmp`.

**It is committed now, with a body that says plainly that it does not compile**
and carries the measured triage, so nobody reads the push as a working state.
That is the correct form for committing a red tree: **the commit is a recovery
point, and its body is the warning label.**

The defence is not discipline. It is a snapshot that costs nothing:

```
snap=$(git stash create "wave snapshot")   # a commit object; HEAD and the
git branch -f <track>-wip "$snap"          # working tree are untouched
git push -u origin <track>-wip
```

`git stash create` makes a commit object out of the dirty tree **without moving
HEAD and without touching a single file** — which is why it is safe to run while
agents are writing, and why it is strictly better than `git stash`, `git add -A`
or a branch checkout, all of which change what is on disk.

**Snapshot before any destructive command, not after.** An hour of integration
is worth one `git stash create`, and the whole cost of the near-miss was that
nobody had run it since the last slice landed. Two tracks lost work to
uncommitted trees tonight — one to a budget cutoff, one to a `git checkout` — and
both were avoidable for the price of a command that does not modify anything.

## Announce the mutation AND ITS CONTROL

A mutation experiment is a claim: *this edit breaks this property and nothing
else.* Most of the time it is true, and the convention has been to announce
**which line you changed**.

**An announcement is not enough, because a mutation can be two mutations wearing
one label.** An agent meant to test arrival order by flipping `pop_front` to
`pop_back` also deleted the `if held.is_some() { return held; }` early return in
the same patch. That is not an ordering mutation — it is "drop the deferred
frames entirely", which is what its *first* attempt had done. **The two runs
produced byte-identical output**, and the only reason it was caught is that
someone compared them and noticed the results matched.

A person who had not diffed the runs would have written "the ordering mutation
is caught" and reported a claim about a mutation they never made. **It is a
measurement of the wrong thing, reported with the confidence of a right one** —
the same shape as a fixture that computes its expectation from the function
under test, and it survived a test that would otherwise have caught it.

**The rule: announce the mutation AND ITS CONTROL.** The control is the thing
that distinguishes *this property moved* from *something else broke*. For an
ordering mutation the control is: **the loss test must still pass, and only the
order test must fail.** Without it, a reader cannot tell a pass that means "the
property is guarded" from a pass that means "you broke the feature harder" —
and the announcement leaves them unable to check the result, only to avoid
stepping on it.

This is the third instance of one shape in this wave — the `x == x` fixture, the
enum field access, and this — and the shape is always the same: **a measurement
that is internally consistent and about the wrong object.** The only defence
found so far is the one that costs something: compare against a control, and
prefer the account that quotes its own output.

## Worker track

| # | Property | File and exact edit | Test that must fail | State |
|---|---|---|---|---|
| W1 | A record is born `Spawned`, not the FSM's retired `None`; a second close is REFUSED | `session/types.rs`, `SessionRecord::new`: `fsm: ChannelFsm::new()` → `default()` | `a_new_record_can_be_attached_and_ends_exactly_once` — **its SECOND assertion**; the attach alone passes under a weaker fix | **NEVER RUN.** The fix for the defect W2a found in the lead's own W-0 file. Most load-bearing row on the branch. |
| W2 | A delta past the row cap escalates to a full frame, never a silent hole | `roost-term/src/emitter.rs:127`: `> LIVE_DELTA_SCROLLBACK_ROWS_CAP` → `> u64::MAX` | `a_delta_past_the_row_cap_becomes_a_viewport_only_full` | unrun. **The only row guarding behaviour inside a COMPLETED ported crate.** |
| W3 | An overflow drops exactly one sink and tells it once | `session/cell_sink.rs::drop_for_overflow`: remove `sink.on_overflow()` | `an_overflow_drops_exactly_one_sink_and_tells_it_once` | unrun |
| W4 | A delta too large for one part escalates to a full | `session/emit.rs::build_frame`: delete the `encoded_cell_grid_frame_size(&wire) > CELL_GRID_PART_MAX_BYTES` escalation | `a_delta_too_large_for_one_part_is_escalated_to_a_full` | unrun |
| W5 | The retained floor stays the offset before the oldest retained byte | `session/types.rs::append_retained`: `head_seq += self.scrollback.len()` instead of `chunk.len()` | `the_history_floor_is_the_offset_before_the_oldest_retained_byte` + 2 | **OBSERVED** — 3 named tests failed, in `734982d1` |
| W6 | A keeper control credential is recognised whatever its case | `shell_spec.rs::is_keeper_control_key`: drop `to_ascii_uppercase()` | `a_keeper_control_credential_is_recognised_whatever_its_case` | **OBSERVED** in `734982d1`. **Re-run:** W1's credential cutover landed after that observation. |
| W7 | A frame from `Box<dyn TerminalCore>` is byte-identical to one from `AlacrittyCore` | the `&dyn TerminalCore` widening in `roost-term` | no test yet — this row needs one written | unrun. A widening that silently changed frame bytes is exactly the defect nobody is looking for. |
| W8 | A spawn's ack is correlated 1:1 with its request under concurrency | keeper pool dispatch | W4's ack-correlation test | unrun |
| W9 | A read against a replaced grid epoch is REFUSED | `retained_grid.rs describe`, the `EpochBinding::new(...)` argument: replace `record.cell_emit.grid_epoch()` with a constant | `a_read_against_a_replaced_grid_epoch_is_refused` | **OBSERVED** (W2c) |
| W10 | A page over the ceiling is CLAMPED, not refused | `retained_grid.rs describe`, the `total:` field: use `origin` without adding the retained count | `a_page_larger_than_the_ceiling_is_clamped_not_refused` | **OBSERVED** |
| W11 | A row is spelled the way the browser already parses it | `retained_grid.rs cell_span_json`, the `fg` insert: skip it when `span.fg == 0`, which is what proto3 JSON does | `a_row_is_spelled_the_way_the_browser_already_parses_it` | **OBSERVED** |
| W12 | An append advances the offset past what the window kept | `scrollback.rs append_pty_chunk`: do **the forbidden write by hand** — `record.scrollback.append(chunk)` plus a manual `head_seq += record.scrollback.len()` | `an_append_advances_the_offset_past_what_the_window_kept` | **OBSERVED**. **The best row in the file**: the mutation *is* the defect W-0's private `history_floor` exists to make unrepresentable, so this row proves the type-level guarantee still holds. |
| W13 | The unhandled log stops at its cap and says it did | `scrollback.rs record_unhandled`, the cap comparison: `>=` to `>` | `the_unhandled_log_stops_at_its_cap_and_says_it_did` | **OBSERVED** |
| W14 | A replay feeds the whole window and says whether it was saturated | `scrollback.rs replay_retained_into`: hard-code `evicted: false` | `a_replay_feeds_the_whole_window_and_says_whether_it_was_saturated` | **OBSERVED** |
| W15 | An alt-screen toggle split across chunks is recognised once | `scrollback.rs scan_stream_state`, the `mode_carry` assignment: `Vec::new()` | `an_alt_screen_toggle_split_across_chunks_is_recognised_once` | **OBSERVED** |
| W16 | A full page takes more than one slice | `scrollback_read.rs SCROLLBACK_SLICE_ROWS`: raise it to the page ceiling | `a_full_page_takes_more_than_one_slice` | **OBSERVED**. Without this a page silently stalls every other session once instead of in slices. |
| W17 | A read stopped between slices leaves no half-taken page | `scrollback_read.rs walk_page`: move the `continue_read` check inside the inner row loop | `a_read_stopped_between_slices_leaves_no_half_taken_page` | **OBSERVED** |

**The trap in W12, and why the row does not stop it.** W2c reached for the
forbidden write — `record.scrollback.append` plus a manual `head_seq` bump —
**twice**, and the reason was not carelessness and not a naming failure. The
capture lane needs to advance the capability-probe carry without retaining
anything; in v2 that was `answerQueries(rec, null, bytes)`, and in v3
`TerminalCore` has no `write_raw`/`get_response`, so that call does not exist. It
was therefore holding bytes, needed the tokenizer carry advanced, found that the
only record method taking bytes was the *retention* method, and — correctly —
concluded the carry and the offset must advance atomically. The one call that
does that is `append_retained`, so it wanted to reach past it for the other half.
Its own words: the argument is not wrong, which is what makes it dangerous.

So a reader who needs to advance stream state without retaining will conclude
the two must be atomic, find that `append_retained` is the only method that makes
them atomic, and reach past it — feeling rigorous the whole way. **The mutation
row does not prevent that. Only a missing method does**, and the missing method
is `advance_stream_state`, which cannot land until the query tokenizer exists.
Landing it earlier would be a `pub fn` with no production caller, which is the
other thing this repo bans.

What stopped W2c was not the rule. It was noticing that `head_seq` and the floor
move together in one expression, so a second copy of it is a second answer to
"when does the floor move" — a design argument, visible only from inside the
file. The lead has since strengthened `append_retained`'s doc to say it is THE
ONLY method that writes the ring or the offset, every lane's including the
capture lane's. **That is not the fix; the seam is.** What it buys is that the
next slice can tell "the method I want does not exist" from "the method I want
is elsewhere" — the distinction W2c had to work out from first principles.

**The transferable lesson is about how instructions are written, not about this
bug.** A brief that said "never write the ring directly" would have produced
compliance and no understanding, and the missing method would still be missing.
An instruction that says *report the pull, not the rule* is what sent someone
looking for the actual cause. The lead's own hypothesis here was confidently
wrong, and offering it as a likely explanation would have buried the real one.
If a later slice reports the same pull, ask for the cause, not for compliance.

**Sweep for the class, run by the integrator.** The pattern is a range or
window whose upper end is derived from the **element's** size rather than from
the **buffer's** length, checked from one side only. Grepped across every crate
for `len() - x.len()`, `len() - N`, `..= x.len()`, `chunks_exact(` and
`.windows(`. **No third instance.** The two candidates in source are both
correct and worth recording why, because "correct" here means something a
reader has to check rather than something they can see:

- `roost-term/src/row_spans.rs:161` does `open = Some(spans.len() - 1)`
  immediately after `spans.push(...)`, so the vector is provably non-empty.
  The two `open` reads around it use `spans.get` / `get_mut`, not indexing.
- `roost-protocol/.../stun_url.rs:162` and `sdp/candidate.rs:155` index
  `bytes[0]` and `bytes[bytes.len() - 1]`, and `bytes[0]` would panic on an
  empty slice — but `is_dns_label` returns early on `bytes.is_empty()` one
  line above, so the guard exists and the arithmetic under it is right.

Everything else the grep returns is `windows(N).any(...)` in **tests**, which
is the correct use of a sliding window for substring search and is not the
class. The one `chunks_exact(` in the tree is the CLI's own SSH argument walk,
and it is there because the lead's predecessor used `windows(2)` there and got
`(VALUE, "-o")` pairs instead of `-o VALUE` pairs.

**The reason a sweep is worth running even when it finds nothing:** both real
instances were found by *someone using the code* — one by a mutation, one by an
audit — and neither by reading. A careful reader checks that a guard exists and
that its arithmetic is right, and in both defects both of those were true. What
a reader cannot check is whether the range *around* the guard was cut the way
its author meant.

**The pattern is wider than I first wrote it, and a third instance turned up in
the coordinator wave.** The general form is **a range end computed from a
claimed or element-derived size, sitting next to a guard computed from the real
buffer length.** Three instances, all found by someone *using* the code:

1. `roost-keeper` `FrameDecoder::push` — a frame's claimed length bounded from
   above only. **Fixed.**
2. `roost-worker` `stream_scan.rs::find` — `haystack[from..=haystack.len() -
   needle.len()]`, so a needle in the final `needle.len()` bytes was never
   found and an alt-screen toggle ending a chunk went unrecognised. Found by a
   mutation, not a read.
3. `roost-coord` `tests/middleware_support/mod.rs:286` —
   `&rest[chunk_start..chunk_end.min(rest.len())]`, where `chunk_end` came from
   a hex size **the peer wrote in the chunk header**. Clamping the upper end is
   not enough when the lower end is derived from the same claim. Test-only, so
   it could not affect a deployment, but it is the shape. **Fixed.**

**Two refinements, both from the people who found them, and both wider than my
original grep:**

- **`.min(len())` on one end of a range is not a bound** when the other end comes
  from the same claimed size. That is instance 3, and my original pattern would
  not have caught it — it has neither `len() - N` nor `..=`.
- **The combination matters, not either arm.** Instance 2 was caught only by
  `..=` *together with* `len() -`. A reader should keep both.

**And the one that is not about ranges at all**, from the same wave: a peer-
supplied value parsed with `.expect()`. `usize::from_str_radix(..).expect("a
chunk size")` two lines below instance 3 is the same mistake in a different
costume — a parse of untrusted input treated as impossible. In a decode path
that is a panic on a malformed message, and the fix is to end the decode, not to
unwind.

## Findings from the read-only audit of the ported crates

The integrator ran a read-only audit of `roost-term`, `roost-keeper`,
`roost-protocol`, `roost-host` and `roost-observability` for the four classes the
wave's own findings demonstrated: a boundary that assumed it was closed, a
derived `Default` producing a terminal state, a trust decision made by an
omitted call, and an executor that silently inherited its caller's state.

| Finding | Severity | Status |
|---|---|---|
| `FrameDecoder::push` bounds a frame length from above only, so a peer sending `00 00 00 00` gets an empty frame and `frame[0]` panics — unwinding the serve loop and `main`, destroying every PTY on the machine. Four bytes from any same-uid process. v2 refused it with a `RangeError` at `protocol-envelope.ts:114`; the port kept the indexing and lost the check, and `LengthMismatch` was declared for it and never constructed. | **CRITICAL** | **Fixed** on `v3` with a lower bound and a test per short length. |
| The keeper applies `spec.env` verbatim with no admission check, while v2 refused a keeper control key in `isShellSpec` **and** again in `mergeEnvironment`. | CRITICAL | **Open.** A worker-side strip is now real, but v2 refused on both sides and the keeper side is the structural one. |
| The keeper's local endpoint has no capability authentication at all; the socket's `0600` mode is the entire boundary. v2 required a verified capability before dispatching any frame, plus a byte cap, a timer and a connection cap. | HIGH | **Open.** Either fix it or record the deliberate drop in the keeper contract and a commit body. |
| Two implementations of the keeper binary digest disagree, so survivor admission can never succeed. | HIGH | **Open.** The keeper-side one is correct; the worker's is not. |
| `keeper_client.rs:295 wait_for_reply` pulls from `self.events` and **discards any frame that does not match** (`:310`) — and `self.events` is not a reply channel. `client_io.rs:42` routes only `SpawnAck`/`SpawnErr` to the pending-spawn map; `PtyOut` goes into the same channel at `:73`, and `server.rs:259-263` proves the interleaving is real. **Every PTY byte the keeper emits between a client request and its reply is lost permanently, with no log and no counter.** A drag-resize is ~60 requests/second. v2's `keeper-pool-lifecycle.ts:194-303` is one `dispatchFrom` loop routing every frame by tag, with output and reply paths disjoint. | **CRITICAL — data loss** | **FIXED and PROVEN.** `wait_for_reply` and `wait_for_any_input_result` now **defer** a non-answer rather than dropping it, and `next_event` drains the deferred queue **before** the socket in arrival order — a frame deferred now arrived before what is on the wire, so reading the socket first would move the hole to the head of the stream. Poisoned locks recovered, not propagated. Proof: `pty_output_written_before_a_control_ack_is_not_lost` (real client, real socket, real handshake, `PtyOut` flushed before the `ResizeAck`, asserting the **exact bytes** — a liveness check would pass against a client that drops the frame and returns a later one) and `output_deferred_by_a_wait_precedes_whatever_arrived_after_it` (the property the fix introduces). **Mutation: `wait_for_reply` dropping the non-answer again fails both, 0 passed / 2 failed.** |
| `KeeperClient` is missing the client half of six frames its own daemon already serves: `GetHistoryRecords`, `GetHistory`, `GetTerminalState`, `ResizeStatus`, `KillChild`, `Shutdown`/`ShutdownIfEmpty`. The tag table is complete (all 34) and the daemon implements every one, so this is client-only. **A worker restarting against a surviving keeper cannot read the history it is supposed to adopt, nor learn the geometry the keeper applied.** `FAILURE-INDEX.md:354` names the user-visible form: history gone after worker restart, pane freezes. | **CRITICAL** | **Open; blocks worker adoption (W2b/W4).** The `KeeperChannels` seam is right — the client underneath it cannot ask. |
| `resize()` discards the `ResizeAck` payload (the **applied** seq and geometry) and collapses nine reject reasons into `ClientError::Io("the keeper refused the resize of channel N")`. v2's `KeeperResizeResult` distinguishes ack/reject/unknown and `session-terminal-txn.ts:188-222` maps all three. Today `resize()` blocks until its ten-second deadline — **with the discard bug open, that entire deadline is spent losing output.** Two findings compounding on one call site. | HIGH | **Open.** Fixing the discard does not fix this. |
| `TerminalCore` has no `synchronized_output` / `synchronized_output_generation`, so DECSET 2026 withholding cannot be ported and the two rescue ceilings in `stream_fence.rs` are unreachable. **The dangerous workaround is to omit the gate**, which makes every full-screen TUI repaint in visible steps instead of atomically. | HIGH | **Open.** Do not "fix" by always passing. |
| `TerminalCore` has no unhandled-sequence reader. v2 reached *past* its own ABI for one — a `WeakMap` plus a raw memory read at hand-computed offsets — so a port reading only the trait concludes "v2 did not use this", which is wrong. `terminal.unhandled_sequence` therefore has no producer. | MEDIUM | **Open.** Telemetry gap: the "wrong in Roost, fine in iTerm" class is unanswerable. |
| `TerminalCore` has no `get_resource_state`, so `terminal.hyperlink_saturated` has no producer. At the core's fixed OSC 8 table capacity, new links render as dead plain text with nothing logged. | MEDIUM | **Open.** |
| `browser_commands::OWNERS` marks `list-skills` and `git-diff` `Absent`; v2 does the same quietly, and no caller exists. v3 answers `rpc-error` where v2 answered silence. | LOW | **Open.** Harmless today; silence becomes a registered-but-unanswered request if a browser ever sends one. The legacy-drop rule says every dropped path is named in a commit body, and nothing names these two. |

**The question that found all of these, and the reason it works:** *what does a
TypeScript caller reach for that this trait does not offer?* It is a question
about the TypeScript, not the Rust — and it is the only one that can find an
omission, because **a trait cannot be audited for what it omits.** Eleven
surfaces were fully enumerated with **no difference** — `ClientControlFrame` 23
variants 1:1, `SessionEvent` 13 variants 1:1, both link unions a superset of
v2's, all seven brand types with `check`/`TryFrom`/`Display`, all 34 keeper
tags, `CoordConfig` and `roost_host::paths` 1:1 — which is what makes the two
`roost-keeper` defects isolated rather than symptomatic.

It also **narrowed** a finding of mine: `write_raw`/`get_response` is one
omission plus one shape constraint, not three, and the per-chunk `afterChunk`
hook is a latency artifact of the WASM build rather than a semantic
requirement. I would have spent a method on it.

The audit also reported the keeper `env_clear` fix as absent. **That was a
false positive**: the audit read `pty_channel.rs` while a mutation experiment
had the line temporarily replaced, and the committed tree carries
`command.env_clear()` at line 134. Recorded here because the hazard is real —
**a read-only review of a tree that is being mutated will read the mutation.**
Pin reviews to a commit hash, not to a working tree.

## Coordinator track — rate limiter (M2)

All four **OBSERVED** in an isolated scratch crate, all restored. Re-runnable as
gate checks without reconstructing the method.

| # | Edit | Test that must fail |
|---|---|---|
| C1 | key the reconnecting caller as `"198.51.100.9#conn-2"` | `a_reconnecting_caller_finds_the_budget_it_left_and_another_caller_finds_its_own` |
| C2 | `spend`: `let key = (method.to_owned(), String::new())` | the same test, plus `a_full_bucket_table_fails_closed_and_recovers_when_its_windows_elapse` |
| C3 | add `"WorkspacesList"` to `RATE_LIMITED_METHODS` | `a_read_neither_opens_a_window_nor_spends_a_mutations_budget`, `a_budget_is_chosen_per_rpc_and_never_inherited_by_a_sibling` |
| C4 | `remaining: rate.tokens_per_window()` on the create path | 5 tests, incl. `the_entry_point_the_mount_calls_answers_on_its_own_clock` |

**C4 caught a real defect before the crate ever linked:** the bucket-opening
request was not spending a token, so the 101st call in a window was admissible.
v2 falls through from bucket-creation to the charge. A port that is *more*
permissive than v2 at a rate-limit boundary is the worst direction for this file
to be wrong in, and at the integrated gate it would have surfaced as "the limit
sometimes does not fire".

## Coordinator track — agent status (AG1)

| # | Edit | Test that must fail |
|---|---|---|
| C5 | `status_order.rs::accepts`, last line: `update.common.revision > held.common.revision` → `true` | `a_late_report_never_displaces_a_fresh_one` |
| C6 | delete the body of `impl Drop for AgentStatusWaiter` in `status_wait/registry.rs` | `a_released_wait_is_not_leaked_when_the_subscriber_is_gone` |
| C7 | `status_push.rs::classify_transition`: drop `&& next.common.completed_revision > previous.common.completed_revision` from the done arm | `only_a_block_and_a_finished_turn_reach_a_phone` |
| C8 | `rpc_status.rs handle_agent_status_wait`: move `AgentStatusWaitRequest::new` validation ABOVE `require_open_agent_status_session` | `a_wait_never_becomes_an_oracle_for_which_sessions_exist` |
| C9 | `config.rs::set_agent_config`: write `selected.to_owned()` instead of the trimmed-or-OMP fallback | `the_default_agent_is_shared_and_a_blank_selection_falls_back` |

## Coordinator track — firehose adapters (SY1)

Six named, one per property the slice exists to protect. The one the brief names
specifically:

| # | Edit | Test that must fail |
|---|---|---|
| C10 | `sync_ws/feed/mod.rs:131`: `Some(&self.meta)` → `None` | `a_frame_queued_and_then_drained_still_carries_its_meta` |

This is contract §12.8's defect — a queued frame carried no meta, so a
reconnecting client got a frame it could not place and the cursor never advanced.
`FeedFrame` has private fields and a `pub(in crate::sync_ws::feed)` constructor so
a frame cannot leave the directory without its meta; this row is what proves the
type-level guarantee survives the next edit.

## Coordinator track — tasks (S2)

| # | Edit | Test that must fail |
|---|---|---|
| C11 | delete the `publish` call in `handle_tasks_set_state` (line 186) | `a_task_change_reaches_a_sync_subscriber` |

The FAILURE-INDEX regression this file exists to prevent.

**Caveat on a sibling test:** `two_devices_never_claim_the_same_task` is
timing-dependent. Run it at `--test-threads=1` several times; one pass is not
proof.

## Known gaps the gate does not close

- **`claim_ttl_ms` is write-only.** A browser that claims a task and closes leaves
  it claimed forever; the wire advertises a bound no reaper enforces. **v2
  behaves identically**, so this is a pre-existing product gap, not a port
  regression, and no reaper is being written during parity. Recorded here so the
  next person reads "a decision is waiting" rather than "a bug slipped through".
- **`ShellSpecResolver` has a trait and no implementation.** Declared at
   `session/spawn.rs:94`, consumed by two finished slices at
  `session/lifecycle.rs:179` and `:217` as `Arc<dyn ShellSpecResolver>`. **This
  is not a compile error — it is a seam that looks complete**, and the gate passes
  over it unless something constructs one. A `SessionManager` that cannot resolve
  a shell spec cannot spawn, and that failure surfaces as a browser unable to
  open a terminal, a long way from this trait. Owner: the `W10SpecResolver`
  slice.
- **`TerminalCore` has no `write_raw` or `get_response`,** so the query
  tokenizer cannot be ported, so `advance_stream_state` cannot land, so
  `append_pty_chunk` stays misnamed for what it does. This is the second of the
  wave's "a method is missing and the thing next to it has to be misused instead"
  shape — W2c reached for the retention method because it was the only one taking
  bytes, and reached for it twice. **The first instance of this shape was a
  naming problem and the second is a missing-method problem, and the difference
  matters:** the first is fixable in a header, the second only by porting the
  query tokenizer. Owner: the worker track, at integration.
- **The device-refusal helper is written MORE than three times.** S2's
  correction: ten definitions now exist — seven byte-equivalent and
  marker-bearing (collapse these), and three divergent (each needs a decision,
  not a merge). The real owner is `auth/principal.rs::require_account_device`,
  which cannot build a `ConnectError` and so is not a copy. `terminal_screen/rpc.rs`
  answers `PermissionDenied` where v2's `auth-interceptor.ts:256-271` answers
  `Unauthenticated` with a marker, which a browser reads as "refresh my session" —
  fix that one first.
- **`workers::rpc::account_device` omits the `x-roost-auth-layer` marker v2 sets.**
  The workers slice's file; lead-owned at integration.
- **`new_task_id` lives in `sessions::tasks`** and S1/S3 may have forked it. The
  integration commit hoists it to a shared coordinator id module. It is a
  deterministic composite of process epoch, boot time and a counter — no RNG, and
  no RNG crate is being added for it.
- **§4.11's route count in `docs/phase3-coord-contract.md` is wrong.** The limited
  set is **32**: 31 string literals plus the `PAIR_POLL_METHOD` alias entry, which
  appears as an identifier rather than a quoted string and is therefore missed by
  counting quotes. v2 also has 32. Three numbers have circulated for this one line
  (44 in the doc, 31 and 32 in relays); 32 is the measured one.
- **§4.11's "LRU maintenance by insertion order" is v2's mechanism, not the
  port's.** The port does `buckets.retain(|_, bucket| now < bucket.reset_at)`,
  dropping what has expired and reaching the fail-closed refusal only when the
  survivors alone fill the ceiling. Same guarantee, different mechanism.
