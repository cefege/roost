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
into the gate file. **The numbering is not the pairing.** Each must-still-pass
was embedded in prose inside its own row, so the two did not line up, and the
real relationships were:

- one row's must-still-pass is a test **in its own prose**;
- one row names a second test that it *expects to fail* — **a must-fail, not a
  must-still-pass**, belonging to that row alone;
- three rows had no second direction at all.

**So: read the row, do not read the sequence.** I applied a pattern to a
numbered list and produced a confident wrong answer — the same error as resolving
a decode function from its name, and the reason the pairing question is
unanswerable by inspection.

**The rule, now with all six rows in it. A mutation row is a claim of the form
"this edit breaks *this* property and nothing else", and it needs both halves to
be a claim:**

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
never adjusted** — the response to an over-broad test is to fix the test, and
"report it" is what stops an agent from deleting the test that got in the way.

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
described but not enumerated has to be either numbered or explicitly parked.**
"Here are six" and "here are six and a seventh I did not number" are different
claims, and only the second one is safe.



Two of six mutation rows from one slice were explicitly **paired**, and the
reasoning is the reusable part:

- **M2 (delete the drain guard) + M3 (delete the emptiness condition).**
  *Together they are the property; separately neither is.* M2 also names a test
  that **must still pass** — and if it also fails, **the test pair is not
  isolating the drain, the second test is worthless, and that is reported
  rather than adjusted.**
- **M4 (delete the drain acquisition) + M5 (delete the stale-credential
  re-read).** M4's failure is expected to hit *two* tests: the first shows the
  property, the second shows what a second concurrent update would do with the
  gate open.

**So a mutation row is not only "this edit must fail this test" — it is some
edits with a named must-still-pass, and the must-still-pass is what proves the
test is isolating the property rather than merely failing.** Deleting the guard
and seeing both tests go red tells you the guard was load-bearing. Deleting it
and seeing one go red and one stay green tells you the second test is actually
pinning the *absence* of the refusal.

**And the row must name a must-still-pass even when there isn't one**, so that
"the mutation changed nothing" is a distinguishable outcome from "the mutation
changed everything". A row that can only fail has not specified what it is
checking.

The other half is the report shape: **if a mutation passes when it should fail,
that is a finding about the test, and the slice that wrote it would rather hear
it than have it absorbed.** "This edit broke nothing" is not a failed experiment;
it is a measurement that the property is unpinned.

