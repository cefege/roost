# The class that has cost this programme more than any crash

**A thing that runs correctly and accomplishes nothing.**

Every other failure mode announces itself. A compile error stops the build. A
failing test goes red. A panic is a stack trace. This class is silent by
construction: the code runs, the check passes, the frame is acknowledged, the
signal is bumped, the bundle ships — and no observable thing differs from the
working case.

It has now appeared on the coordinator, the worker, the web track and in the
gate definitions themselves. This file records the instances so the next reader
sees the pattern rather than the sixth example of it.

## The instances, in the order they were found

### 1. A `#[test]` that stopped being a test — coordinator/worker/web track

A split dropped a `#[test]` attribute. The function still compiled, still sat in
a test binary, and **cargo never ran it.** It shipped, because `cargo test` does
not fail on warnings and `clippy -- -D warnings` had never been run on the tree.
The absence of one check hid the failure of another.

### 2. `clippy -p roost-web-terminal` failing on a crate it never named

`roost-client-core` is a workspace **path** dependency, so cargo does not apply
`--cap-lints allow` to it. `clippy -p roost-web-terminal -- -D warnings` stopped
in client-core's lib — the one crate that command does not name. Linting one
crate required the other to be clean first, and nothing said so.

### 3. A finding list from one clippy run — a lower bound, not a count

Clippy is **fail-fast per compilation unit.** Four findings were reported; fixing
the fourth revealed a fifth in a different test binary, and fixing the fifth
revealed two more. The true count was six. A sibling agent hit the same shape
three times independently. **On a tree that has never been linted, "that is all
of them" is a claim about the run, not about the tree.**

### 4. 414 lines of DOM adapter that had never been through a compiler

`src/input/dom.rs` and `src/links/dom.rs` are `#[cfg(target_arch = "wasm32")]`.
A native build skips them. Host-target `clippy --all-targets` skips them. The
formatter only reformats. **Parts 1, 2 and 3 of the gate were all green over
them.** Part 4 — the one never attempted — is the only one that could ask.

### 5. A gate green because the crate was absent

The U-2 gate named `roost-client-core` and `roost-web-terminal` in both its test
and build commands, and never named `roost-web` — so the entire UI crate, and its
39 tests, were outside it. `ci.yml:39-42` had already written the lesson down:
*"A gate that is green because it is absent is not a gate."*

### 6. A reverted-caller check that passed on exactly the state it should have caught

A commit was verified as "byte-identical to the previous commit" — where the
previous commit **was** the one that made the change. The assertion and the state
it was meant to falsify were the same object. The check could only ever confirm
those files had not changed since the commit that changed them.

### 7. A revision signal nobody reads during render — the pump

Dioxus re-runs a component when a `Signal` it **read during render** changes. A
`Signal<u64>` bumped after every `ClientEvent` and read by nothing bumps forever,
repaints nothing, and builds green. The read is part of the design, not a
follow-up — this is the specific trap the whole pump slice is shaped around.

### 8. `SyncFrame::DomainReady` constructed by a flat `match` — compiles, wrong

`sync.proto:311` puts `SyncDomainReadyCommand` inside `SyncClientFrame`
(client → server), **not** in `FirehoseFrame`'s oneof. The client sends it and
derives `SyncFrame::DomainReady` from the reply. A decoder written to the shape of
the **enum** rather than the shape of the **wire** compiles, leaves `DomainReady`
unconstructible, and never closes the terminal domain's snapshot/live gap.
Nothing is red.

### 9. `AgentStatus` decoding as `Unknown { field: 29 }` — acknowledged, discarded

`inbound.rs:111-118` records it: without the variant, every agent report decoded
as an unrecognised frame — "sequenced and acknowledged, and applied to nothing."
This one is **worse than a refusal**, because a refusal is visible and this is
invisible by construction. The coordinator believes it delivered; the client
acknowledged so the window keeps releasing; nothing happened.

### 10. Three production paths handle an event nothing can produce

`SyncFrame::DomainReady` has **four consumers in production code**
(`handle_sync.rs:38`, `sync/domain.rs:158` and `:169`,
`handle_sync/apply_frame.rs:34`) and **exactly one producer in the entire
workspace: a test helper** (`tests/support/sync_reconnect.rs:77`).

There is no wire source for it. `sync.proto:311` puts `domain_ready` in
`SyncClientFrame` (client → server); `roost-coord`'s `handle_domain_ready`
(`sync_ws/commands.rs:196-243`) consumes the one-time token, sets `state.ready`
**server-side**, calls `request_flush()`, and returns a **local**
`CommandOutcome::DomainReady` — it sends no frame back. The client learns the
domain is ready because that domain's frames start arriving.

So the shape is new even within this class: not a check aimed at the wrong scope,
but **code that handles a case the system can never enter**, kept looking covered
by a test that constructs the value by hand. Three code paths are unreachable and
their tests pass.

The only wire-visible failure is the inverse — a token that does not check out
**resets** the domain (`commands.rs:221, :224`, `reset_terminal`), which arrives as
a decodable `DomainReset { reason: "snapshot_token_invalid" }`. A client has to
read readiness from *resumption of traffic* and from *resets*, never from a
readiness frame, because no such frame exists.

### 11. A linter's success word reads as an action it did not perform

`cargo xtask fmt` reports its two outcomes in different grammatical moods:

- **failure** — prints each diff and the instruction `xtask fmt: run \`cargo fmt
  -p <crate>\` over the workspace and commit`. Unambiguously a report.
- **success** — prints **`xtask fmt: formatted`**. Past tense, an action.

On the success path nothing was rewritten. `git status` showed 0 files and
`git diff --exit-code` exited 0, because `xtask fmt` is a *check*: it reports
what it would change and leaves the tree alone.

So the tool's own wording asserts a mutation it did not perform, in the one
direction where nobody is looking — a reader who has just been told the merge
cleared three violations reads "formatted" as "and here is the diff it produced".
The check that caught it was not reading the word; it was running `git status`.

This one is worth keeping for the general rule it implies: **a tool's output is
evidence about the tool, not about the tree.** Where a check both reports and
acts, only `git status` distinguishes the two, so that is the measurement to
quote — never the tool's own verb.

### 12. A path that runs correctly end to end and signals nothing — coordinator

`handle_domain_ready` (`roost-coord/src/sync_ws/commands.rs:196-243`) does every
step right: it validates the domain and generation, consumes the one-time token,
sets `state.ready = true`, calls `session.request_flush()`, and returns
`CommandOutcome::DomainReady`.

**And both of its outputs are unread:**

- `CommandOutcome::DomainReady` is **constructed at `:241` and matched nowhere**
  in `sync_ws`.
- `request_flush()` sets `flush_requested`, which is read only by
  `take_flush_request()` — and **that has no callers.**

So a client that sends `domain_ready` to be hydrated raises a flag no code path
observes and returns a value no `match` consumes. **Every step is correct and
the composition delivers nothing.**

This is the most literal form of the class: not a check aimed at the wrong
scope (1-7, 11), not an unreachable handler (10), and not a value nothing
produces (10) — a **complete, correct path whose entire output is unread**.

**What is NOT claimed, and the reason the finding is usable:** hydration is not
known to be broken. `drain_terminal_queue` *is* called (`send_queue.rs:183`,
`:204`), so queued frames do go out. The open question is whether that drain is
gated on the dead flag or unconditional on the socket's write tick. Three
answers, three actions: **unconditional** means all three symbols are vestigial
and get deleted; **gated somewhere not yet read** means hydration never happens
and it is a real defect; **a third carrier reading `state.ready`** means both
symbols are redundant *and* the real signal is undocumented. One grep, no
compiler.

Second-order, and the more useful half: **whatever the coordinator sends after
`domain_ready` is not sent by the `domain_ready` handler.** So
`SyncFrame::SessionsSnapshot` has no identified source *and* a second reason not
to assume one — an implementer who picked the `Frame::Sessions` arm because it
was the convenient candidate would be building on a path that sends nothing.
**Located, not designed.**

## What they have in common

Each is a place where **something reports success about work it never did**:

| instance | what reported success | what it did not do |
|---|---|---|
| dropped `#[test]` | green test binary | run the test |
| `clippy -p web-terminal` | a lint command | lint the named crate |
| 4 findings | a clean run | see the other units |
| 414 wasm lines | green parts 1–3 | compile the code |
| gate | green parts 1–2 | cover the crate |
| revert check | a pass | compare against the right commit |
| revision signal | a green build | read the signal |
| `DomainReady` | a compiling decoder | close the snapshot gap |
| `AgentStatus` | an acknowledgement | apply the report |
| `DomainReady` handlers | a passing test | emit the event they handle |
| `xtask fmt` | the word "formatted" | rewrite anything |
| `domain_ready` | a correct code path | read either of its two outputs |

## The question that catches it

**What did this instrument actually look at?**

Not "did it pass" — every one of these passed. *What was in scope, and what was
silently outside it?* A named crate, a named target, a named file, a named
commit. Each of these is a scope question, not a correctness question, which is
why they survive review: the code is fine, the check is fine, and both are aimed
somewhere other than where the risk is.

## The standing rules that fall out

1. **Prefer a diff to a count, and a diff against the state you are claiming
   about — not against the neighbouring commit.** Instance 1 and instance 6.
2. **Treat any single instrument's result as a lower bound the first time it runs
   on a tree.** Instances 3 and 4.
3. **Name the scope in the gate itself, and check the gate's scope against the
   tree's.** Instance 5.
4. **A construct that no read path consumes is not a feature, and a handler
   whose event nothing emits is not coverage.** Instances 7, 8, 9, 10.
5. **A refusal is better than a silent drop.** Everywhere.
6. **A test that constructs a value by hand can make an unreachable path look
   covered.** Check that production can produce what the test produces —
   instance 10 is a test helper papering over three dead production paths.
7. **A path that runs correctly and whose output nothing reads is worse than one
   that was never written.** Instance 12 is the only case here where every step
   is right and the composition still delivers nothing, so the habit that
   catches it is not "did it work" but **"who consumes this?"** — for an enum
   variant, a flag, a returned value, or a rendered element.
