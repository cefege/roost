# Recovery is not recovery. Read this before reading a diff.

Everything in this directory is the output of an accident, and every file in it
is a **draft**. Not one of them is a deliverable, and the fact that it compiles
is not a reason to treat it as one. This file exists because the manifest
beside it documents *what the operations were* and says nothing about *why the
result is not something to merge* — and the second half is the part that has to
outlive the person who worked on it.

Read this first. Read the manifest after. The order is deliberate: the manifest
is interesting and this file is not, and a reader who starts at the manifest
will arrive at the diff already impressed.

---

## The rule

**A recovered artefact is a draft. It is not a deliverable, and the fact that it
compiles is not a reason to merge it.**

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
and the shape is the cheap half. Anyone reaching for a transcript should
assume the result is a starting point for a human-readable diff, not a
shortcut past writing the code.

---

## What these branches are for

They are **references, not candidates.**

- `workerroot-replay.txt` — the manifest of sixty operations, in transcript
  order, for anyone who has to rebuild or audit a specific edit.
- `recover/workerroot` — the output, on its own branch, never merged. A future
  slice may read the ten-step boot composition, the `deps()` shape, the
  `ChannelDelivery`-over-a-shared-emitter wiring, or the shape of the
  replayability gate, take what is right, and write the rest against the
  current tree so the final diff is something a person can read.

That is a better position than the one this started from, and it is not the
same thing as a merge. The 2W path — keeper client, enrollment, durable store
and senders, snapshot and reconcile, browser-command deps, local door, then
the boot order — is how `UNIMPLEMENTED` reaches zero, and it produces code
that was reviewed as it was written.

**Do not merge a recovery branch. Do not let a green build on one substitute
for a review.** If a piece of it looks worth having, port the idea and write
the code again.
