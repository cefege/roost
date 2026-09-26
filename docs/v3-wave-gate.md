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
