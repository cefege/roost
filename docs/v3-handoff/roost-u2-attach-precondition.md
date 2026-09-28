# U-2 ATTACH — preconditions, and one trap that must be read before any code is written

Written 2026-09-28 by the integrator, from WebLead4's producer-side read on
`crates/roost-client-core`. Nothing here is implemented. No commit is owed for
this document, and none should be.

## 0. Read this first: the two arms are a trap, and they are not a defect to fix

`client/attachments/direct.rs:230` and `:247`:

```rust
RouteOpen::Refused(_) => {}                       // :230, loopback arm
RouteOpen::Refused(_) => DirectAttempt::Unavailable(
    DirectUnavailableReason::PeerRefused),          // :247, peer arm
```

**`RouteOpen::Refused` has no production constructor. The two match arms are the
only occurrences of the variant in the entire source tree.** Nothing builds it;
only test support does, at `tests/attachment_support/mod.rs:86`.

So these are not a fail-open guard that is currently open. They are **match arms
written for a state the port's own carriers cannot produce** — and `RouteOpen`
itself is a Rust outcome type with **no v2 counterpart**, which is a separate
observation from the arms being unreachable.

### The premise the comment asserts, and nothing checks

Above `:247`:

> *"Both routes were refused before any byte left, so the relay is still an
> untouched carrier rather than a second copy of the file."*

"Before any byte left" is a premise about the `sent_chunk` flag, and
`AttachmentTransferCarrierError::refused` (`transfer.rs:59-63`) **hardcodes
`ambiguous: false`** and threads `sent_chunk` through. v2's actual gate is
`attachmentDirect.ts:117`:

```ts
if (error instanceof AttachmentTransferCarrierError && !error.sentChunk
    && !connection?.sentChunk) return null;
throw error;
```

**So the flag that decides fallback is `sent_chunk`, and the arms drop it.** If
U-2 ATTACH ever teaches a carrier to answer a timed-out acknowledgement with a
route-level outcome, `:247` will silently map *"may already have committed"* to
`PeerRefused` — an untouched carrier — and the author will have read the
comment above it, which explains the behaviour confidently.

## 1. What IS reachable today, and it is not the same thing

Ambiguous carrier errors **are** produced in production, today, on an
acknowledgement timeout:

```
client/attachments/direct/loopback.rs:186   .fail_ack("… acknowledgement timed out", true)
client/attachments/peer.rs:225              .fail_ack("… acknowledgement timed out", true)
```

which is v2's `rejectAck(waiter, reason, true)` at `attachmentTransfer.ts:98`.
(`send_failed` at `loopback.rs:126` and `peer.rs:171` pass `false` and are
**correct** — a write that threw before writing provably sent nothing.)

**An ambiguous error currently escapes as an `Err` from the route rather than
being mapped at all.** That is the actual shortcut: the port never built the
route-level outcome that would carry the flag. It is not that the outcome is
mapped wrongly; it is that it does not exist.

## 2. Preconditions for whoever implements ATTACH

1. **Do not add a `RouteOpen::Refused` constructor.** It has no v2 counterpart
   and no producer; adding one lights up an arm that is already wrong, for a
   shape the port does not need. *(WebLead4 proposed exactly this and withdrew it
   on the producer read — recorded here so the next person does not re-propose
   it.)*
2. **v2 has no ambiguous-refusal outcome at all.** `attachmentTransfer.ts:93`
   does not rethrow an ambiguous carrier error; it enters `recoverAcknowledgement`
   (`:111-131`), which asks the carrier's status, then the coordinator's, and
   synthesises an ack via `acknowledgementFromStatus`. **The recovery must exist
   before an ambiguity can become an outcome — the current shape is a guess, and
   a guess is what this whole programme exists to prevent.**
3. **The recovery is larger than the ~40 lines of v2 it mirrors.** The ambiguous
   state is currently **inexpressible** — `refused()` hardcodes it false — so
   there is nothing to recover from until a constructor exists. The constructor
   is part of this slice, not a prerequisite handed to it.
4. **`does_not_resume_a_nonfinal_direct_upload_through_coordinator_status` and
   this are ONE missing subsystem, not two red tests.** Do not spend a slot on
   either independently.
5. **`receipt.rs:3` already says "Ported from `recoverAcknowledgement`" and has
   no code under it.** That doc comment is a claim about unwritten code, and it
   is the cheapest possible trap for the next reader of that file.

## 3. The two instruments this produced

**A comment asserting a premise is a claim about code you have not read, whether
or not that code is currently reachable.** *(WebLead4's phrasing, and better
than the integrator's. Five instances so far; the difference between them is
only whether the code runs yet.)*

**A variant constructed ONLY by test support is unreachable in production.** For
each enum variant, list every occurrence of `Enum::Variant` and read the list:
if every construction is under `tests/` or in a `*_support` module, then no
production code can produce it, and its match arms in `src/` are unreachable.
This is mechanical, needs no compiler, and is a candidate `xtask lint` rule of
the same shape as the existing fixture-allow rule.

**The `tests/` / `*_support` exclusion is LOAD-BEARING, and stating the rule
without it produces a rule that passes on the very case it was written for.**
`RouteOpen::Refused` **is** constructed — at `tests/attachment_support/mod.rs:86` —
so a plain "never constructed" grep finds that construction and passes. The test
support exists precisely to make an otherwise-unreachable state expressible, so
counting it as a producer is exactly the error. *(Corrected 2026-09-28 after the
integrator wrote the rule without this clause; the integrator's own measurement
script had the same bug, counting `attachment_support` as production because it
is not the declaring file.)*

## 4. Status

`45af7b4a` stands. 352 passed / 7 failed on `v3-web` @ `dd32c4e9`. No commit
written for this finding, deliberately: **the repo's own rule for a
`docs/FAILURE-INDEX.md` entry is "a NEW root cause is confirmed AND a regression
test exists for it" — there is no test, because there is no code change. So this
is a track spec, not an index entry, and it becomes an index entry when ATTACH
lands with its guard.**
