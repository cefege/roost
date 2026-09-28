# Recovery is not recovery. Read this before reading a diff.

Everything in this directory is the output of an accident, and every file in it
is a **draft**. Not one of them is finished work, and the fact that it compiles
is not a reason to treat it as reviewed. This file exists because the manifest
beside it documents *what the operations were* and says nothing about *what
state the result is in* — and the second half is the part that has to outlive
the person who worked on it.

Read this first. Read the manifest after. The order is deliberate: the manifest
is interesting and this file is not, and a reader who starts at the manifest
will arrive at the diff already impressed.

---

**Status of `recover/workerroot` as of `7c127289`: not merged, and not
permanently unmergeable.** It carries the boot composition that closes the
two `UNIMPLEMENTED` markers `v3-worker` still has. It is waiting on a review,
not on a verdict. The last section says which.

## The rule

**A recovered artefact is a draft. It is not a deliverable until someone who
did not write it has read it, and the fact that it compiles is not a reason to
merge it.**

The half of that rule which survives any review is the last clause. The half
which does not is the implication that the answer is to re-derive the work
from scratch, and the last section says why that is the same mistake taken
twice.

The work this directory holds was not written once. It was written by an agent,
erased by a revert, and then reconstructed by replaying a transcript of the
edits. Every line in it is a line somebody typed under a *different* plan,
before the blockers those lines were written against were known, and then threw
away. What the replay returned is the bytes. What it did not return is the
review — and the review is the part that was never recoverable, because it
lived in the head of the agent that was erased.

So a recovered tree arrives with one witness to its correctness: a compiler.
A compiler proves a type checks. It does not prove a lifetime is right, a
refusal branch is reachable, or a piece of state is the one the author meant.
Those are three different questions and a green build answers none of them.

---

## Three times tonight the compiler and the design disagreed

All three were found by *reading*, not by building. That is the whole argument.

### 1. The gate was justified by a claim that was false

The replayability probe in front of `SessionManager::adopt_survivor` was
requested because of a claim repeated across briefs, a state file and a night
of planning: *`adopt_survivor` can never succeed against a real keeper, and
its refusal path kills the survivor, so a boot that adopts survivors destroys
every live terminal on every restart.*

Measured against a real keeper with a real PTY, with the probe disabled: **the
child is still alive.** `adopt_survivor` does call `abandon`, and so
`keeper.kill_channel` — but on three of its six refusal paths
(`session/resume.rs:225`, `:232`, `:251`), and the `channel_history` refusal
that always fires against a real keeper is *not* one of them. It returns
through a plain `map_err` at `resume.rs:204-207` with no `abandon` at all.

The claim was wrong. The **feature** survived its own justification being
falsified and is still correct — `adopt_survivor` has kill paths at all, which
is reason enough to put something in front of it — but the reason it was asked
for was never true, and a build said nothing either way. The real damage is
quieter than the story: `deliver_into` at `resume.rs:201` runs *before* the
history request and has already rebound the channel's output to a staged
binding, so a survivor is silently orphaned rather than killed. Finding that
took reading sixty lines of `resume.rs` and a mutation run. Nothing in the
build would ever have surfaced it.

### 2. The error count was a lower bound wearing a total's clothes

"Twelve compile errors" was quoted in the task list, the state file and a
handover. It was **twelve in the lib target**. `cargo check --all-targets`
reported **twenty-seven**; the other fifteen lived in test targets the narrower
run never compiled, and one `use` statement in a test module accounted for
eighteen of them.

Every one of the twelve was real. The figure was not wrong, it was scoped to
something that had not been said, and it travelled as if it were the whole
count. This is the same shape as a clippy run that stops at N: it has told you
about N errors and nothing about what is behind them. Expect a fourth round.
The count here fell 27 → 3 → 1 → 1 → 0 and never rose, but that was luck about
the ordering, not a property of the work.

### 3. A body everyone assumed was missing was there the whole time

Eighteen errors across `channel_delivery.rs` read like a missing
`impl ChannelDelivery for TableChannelDelivery` — a structural blocker quoted
in three places, the kind of gap that makes a file a declaration rather than
an implementation. The body is present and complete at
`channel_delivery.rs:101-189`, with the `holding` helper at `:191-196`. All
eighteen were one missing `use` in `mod tests` that put the trait out of
scope, so three method resolutions failed at once in five places.

The inference was made from the *count* (fifteen unexplained errors) to a
*cause*, and the cause reached for was the one that fit the story already
told about the replay being incomplete. The evidence was eighteen line numbers.
A build had answered a question nobody asked it.

---

## The most dangerous result of the night

The work was not lost. It was recoverable — from a transcript, by replaying
sixty recorded operations, in an afternoon.

**That is the problem.** It works, it is cheaper than rewriting, and it
produces output that *looks* finished: it compiles, it has tests, its test
names are plausible. A team that learns work can be recovered from transcripts
stops protecting work, because the failure stops being real. And the recovery
is always more expensive and less complete than the thing it replaces —
seventy replayed operations produced a tree whose compile errors outnumbered
the original slice's, whose `deps()` method had been lost and had no
construction site for the production `Deps`, and whose safety argument was
falsified.

The recovery bought back the **shape**. It did not buy back the **judgement**,
and the shape is the cheap half. A recovered tree is worth having — it is a
week of work in an afternoon, and it gives a reviewer something concrete to
disagree with. It is not worth *trusting*, and the two are different claims
that the word "recovered" runs together.

---

## What these branches are for

**They are drafts awaiting review, not finished work and not permanent
non-candidates.** Both halves of that matter, and the second one was got wrong
here first.

- `workerroot-replay.txt` — the manifest of sixty operations, in transcript
  order, for anyone who has to rebuild or audit a specific edit.
- `recover/workerroot` — the output, on its own branch, carrying the boot
  composition that closes the two `UNIMPLEMENTED` markers `v3-worker` still
  has. It is **not merged**, and the reason it is not merged is that nobody
  has read it — not that it is known-bad.

### The answer to "this has not been reviewed" is a review

The obvious move from a one-witness tree is to re-derive the work from
scratch, and that move is wrong for a reason worth writing down: it repeats
hours of the same work and lands in **exactly the same unreviewed state**.
The track is no further along, and the appearance of rigour was bought with
the critical path. A rewrite is not the rigorous option. It is the same
option, taken twice.

So the sequence is: **review the recovered tree, fix what the review finds,
then merge behind the track's own gate.** A reviewer who did not write the
code is the point — the author of a slice is the one witness already known to
be unreliable, which is a claim about the code's origin, not about the person.
Parts nobody has read, on this branch specifically: the replayability probe
and its refusal classification; the `deliver_into`-before-probe ordering
against `resume.rs:201-207` and whether the staging buffer is bounded on every
path; the restored `SessionStack::deps`; `TableChannelDelivery` over the
shared emitter, and why two types rather than one impl; and the ten boot
steps in the function body, whose order is defended by nothing —
`tests/worker_boot_order.rs` asserts against `boot_order.rs` only and never
reads the function.

### What the 2W path is for

The 2W slices — keeper client, enrollment, durable store and senders,
snapshot and reconcile, browser-command deps, local door — are how
`UNIMPLEMENTED` reaches zero **and** how the code gets read as it is written.
They are not an alternative to reviewing this branch. They are the reason a
merge of it would not be a lucky escape from the track: the rest of 2W is
still to be written, and it will be written against whatever this branch
turns into.

### What is still true whatever the review finds

**Do not let a green build on a recovery branch substitute for a review.**
That is the standing rule, and it holds after a successful review too — a
reviewer is a witness, not a certificate, and the second reader of a
recovered tree should assume the first one missed something.

**And the line numbers in this file are the first thing that goes stale.**
`resume.rs` in particular will move. Every citation here was checked against
the tree when this file was written, which is the only guarantee it carries
and not a durable one.
