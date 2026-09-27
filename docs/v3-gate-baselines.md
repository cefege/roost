# v3 gate baselines

Every phase gate in the Rust rewrite runs the same Playwright suite that
already guards v2. A gate that only says "all tests pass" cannot tell a Rust
regression from a suite that was already red, so each tier records what the
**all-TypeScript** stack does on the same machine, and a later gate is read
against that number.

**The rule this file exists to enforce:** a spec "passes at this gate" only if
it passes in the baseline below. A spec that skips in the baseline must skip
for the same reason; a spec that is newly skipped is a gate failure dressed
up as a non-event.

## Baseline: the all-TS stack

`bun run test:terminal` on `main` (`96f4db25`), Linux x86_64, 8 cores, run
while the four Rust tracks were compiling. One correctness pass at Playwright's
own worker sizing, then one serial perf pass — the same two passes
`bun run test:terminal` performs, unmodified.

| Pass | Collected | Passed | Skipped | Failed | Wall |
|---|---|---|---|---|---|
| correctness | 145 | 142 | 3 | **0** | 12.3m (4 workers) |
| perf (`@serial`) | 18 | 15 | 3 | **0** | 45.7m (1 worker) |
| total | 163 | 157 | 6 | **0** | 58.0m |

**The baseline is fully green.** The rewrite plan expected
`terminal-peer.spec.ts:61` ("loopback wins before a WebRTC peer is allocated
and keeps Sync metadata live") to fail; it passed in 20.1s. Every later gate
is therefore held to the full 157, with zero tolerance.

### The six skips, and why each is not a Rust gap

Every one is a self-skip inside the spec, keyed to a capability this Linux
box does not have. Each must keep skipping for the same reason; a Rust gate
that makes one of them *run* is a change in platform capability, not a fix.

| Spec | Why it skips here |
|---|---|
| `composer-mobile-input.spec.ts:86` | mobile composer preserves and submits terminal input — mobile viewport capability |
| `terminal-mobile-font-width.spec.ts:260` | WebKit iPhone text inflation — macOS-only WebKit engine, per `playwright.config.ts` |
| `voice-dictation.spec.ts:25` | desktop passive drafting preserves a selected reader — needs the dictation/mic path this box does not expose |
| `perf.spec.ts:282` | trusted key, shallow/deep reveal, child-observed resize |
| `perf.spec.ts:286` | optimistic first marker paints while spawn response is held |
| `terminal-switch-perf.spec.ts:94` | activating a pane that moved while inactive costs one complete baseline |

The last three are perf cases gated on the same precondition as their
sibling cases in those files, which do run.

## Deviation: one spec did not reproduce on `v3`, and the baseline is NOT restated

`bun run test:terminal` on `v3` @ `9b9611f1` (the TS stack, `ROOST_SMOKE_WEB_DIST`
unset) returned **141 passed / 1 failed / 3 skipped** in the correctness pass,
against this baseline's **142 / 0 / 3**.

The failure: `[firefox-peer] › smoke/terminal/terminal-peer.spec.ts:263` —
*"invalid offers, unavailable grants, expired grants, and identity mismatches
fall back without recreating the PTY"*, `@serial`. The stack log line for that
run reads `smoke stack: coordinator=typescript worker=typescript
web=apps/web/dist`, which is the default the knob is required to preserve.

**Reproduced twice, in isolation, and it passed both times**: 1 passed in 45.1s
and 1 passed in 43.0s, same project, same `-g` filter, `--reporter=line`.

**That is not a finding, and the baseline is not being restated.** What the
isolation runs establish is narrow: the spec is not reproducible in isolation.
What they do **not** establish is that it is a flake, and the difference matters
because a claim of "flake" is a claim about cause that nobody has evidence for
yet. The full run put four Playwright workers plus four Rust tracks' cargo on
eight cores — load average 25 to 30 during the run, against roughly 8 for the
isolation runs. A WebRTC offer/grant timeout is exactly the shape that load
breaks and that recovers on its own, so load is a plausible cause and not a
measured one.

**The rule for reading this later.** A later gate is held to the full 157 with
zero tolerance, and this deviation is **unresolved, not forgiven**. The next
full `test:terminal` on this tree must either come back 142/0/3 — which closes
it as non-reproducing under load — or reproduce it, which makes it a real
failure to diagnose. Do not read the two green isolation runs as a cleared
gate; read them as the only evidence that exists, and note what it does not
cover.

## The integrator tree's own gate, after the bootstrap commits

`v3` @ `1c9579e8`, Linux x86_64, 8 cores, run while four track worktrees were
compiling in their own target directories. This is the number the integrator
publishes, and it is the only figure in this file that measures the Rust tree.

| Gate | Result |
|---|---|
| `cargo test --workspace --no-fail-fast` | **1437 passed / 0 failed / 0 ignored** across 173 binaries |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean**, confirmed on two independent runs |
| `cargo xtask lint` | **0 violations**, `checked 880 inputs`; 35 `xtask` tests pass |
| `cargo xtask fmt` | formatted, working tree unchanged |
| `bun x tsgo -p tsconfig.base.json --noEmit` | **clean** |

**Up from the 1420 the plan recorded at `a464fa3e`**, and the difference is real
work rather than drift: the `AgentStatusOrder` ordering module and its tests, the
`lint_table` rule, and the `xtask` self-tests that came with the DAG and design
changes.

**Two things this number is not.** It does not cover the four track branches —
each is mid-wave, and merging any of them turns it red for reasons that are
progress rather than defects. And it was measured **before** `roost-keeper`'s
lint-table copy lands, which puts that crate under four lint groups it has never
faced. The carry list predicts that merge turns the workspace clippy red, so **a
clean workspace clippy on `v3` is a statement about `v3` and not about the
programme.**

## Where each track stood after it took `v3`, 2026-09-27

Every track branch was merged to `v3` on 2026-09-27, and each was then
measured with `cargo check --workspace --all-targets` in its own worktree. This
is a COMPILE count, not a test count: it is the lower bound a track lead starts
from, recorded so a later track number can be read as progress rather than as
a fresh surprise.

| Track | Tree | `cargo check --workspace --all-targets` |
|---|---|---|
| CLI | `v3-cli` @ `7fbea312` | **0 errors** |
| Worker | `v3-worker` @ `8a85f523` | **0 errors** |
| Web | `v3-web` @ `13ce9833` | **51 errors**, all in `roost-web-terminal` (lib and lib test) |
| Coordinator | `v3-coord` @ `3b043a6b` | not re-measured; its work is already merged into `v3` |

**The web number is a real defect and it was invisible to CI.** `roost-web-terminal`
fails with the SAME 51 errors on `wasm32-unknown-unknown` as on the host, so it
is not a `cfg`-gating gap — it is a half-finished refactor. The renderer was
being split into sibling `impl` files (`cell_renderer/{scrollback,eviction,
history_page}.rs`) and those modules call `CellGridRenderer` methods and types
that do not exist on the parent; `web-sys` features for `Element::{children,
style,class_list}` are missing from the manifest. It went unnoticed because
`ci.yml`'s wasm job builds neither `roost-web` nor `roost-web-terminal`.

**The `kill(-1)` test, run before any `cargo test -p roost-cli`:** commit
`56a6bb59` is an ancestor of every track branch, so
`crates/roost-cli/src/dev/signal.rs` is the fixed version everywhere. The
dangerous version of that file exists on no branch in this family.

## Track gates as they land

### CLI: `v3-cli` @ `b35f0f65`, merged into `v3` as `0ad703be`

`cargo test -p roost-cli --no-fail-fast`, **two agreeing runs**: all 37
`test result:` lines read `ok`, 0 failed anywhere, both times. The baseline on
this branch was **17 failures across 11 binaries**.

**The merge itself is verified, which is a separate claim from the suite.**
`cargo check --workspace --all-targets` on `v3` @ `0ad703be`: **0 errors**,
finished clean. That merge carried 24,231 insertions across 116 files including
a 45-file rustfmt reformat, and a green suite on the branch says nothing about
whether the merge broke a sibling crate. This is the check that says so, and it
is cheap next to a full test build, so it is the one to reach for after any
track merge rather than discovering the answer inside a gate.

**Thirteen warnings came over with it, and they decide the clippy gate on their
own** under `-D warnings`: unused `code` (`api/agent_prompt.rs:102`), `args`
(`api/scrollback.rs:109`), `row` (`api/ui.rs:37`) and a missing `Debug` on
`api/client.rs:42` and `quickstart/install.rs:55`; unused `with`; and unused
imports or bindings in `deploy_release_path.rs`, `deploy_release_stage.rs`,
`update_recovery.rs` and `update_self_replace.rs`. They are pre-existing on
`v3-cli` and owed there, and the fix is to delete the dead binding rather than
to allow the warning away — an allowed warning is a lint rule that has stopped
existing.

The 17 were **6 product defects and 11 wrong expectations**, and the split held
under re-reading. It is worth recording WHY, because "17 red" reads as a broken
port and it was not: the product defects were a payload preview that trimmed
where v2 collapsed in place; a dial-URL resolution that asked
installed-then-ambient *per name* so a shell exporting the most specific name
outranked the installed definition and silently enrolled the next machine at the
wrong door; a service spec that took `GIT_SHA` from the running binary's
compile-time stamp, so every definition named a commit nobody installed; a
failed commit decision exiting 1 instead of 8, which a wrapper reads as
"retry the same transaction"; and — the one that would have broken the cutover —
booleans written with Rust's `Display` (`ROOST_TRUST_PROXY=true`) where the
loader reads `== "1"` and `parse_terminal_peer_enabled` refuses `true`
outright, so installs came up with the wrong front-door policy or would not
boot at all.

That last one had a second defect behind it, found by following the failure one
assertion further rather than stopping at the fix: the Cloudflare Access pair
was written BLANK when unset, and the host refused a blank team domain while
`normalize_https_origin` reads a blank public URL as unset. **Every coordinator
installed without Cloudflare Access got a definition it could not boot from.**
The root cause was in the host loader and is fixed there; the CLI omission is
kept as v2 parity. A test that stops at the assertion it was given will find
the surface, not the cause.

`clippy -D warnings` and `xtask lint` were **not run** on that branch — the
build queue was spent getting the push out. Six pre-existing `roost-cli`
warnings are still owed there, and `xtask lint` has never been run in its
workspace form by anyone.

### Web: the first published gate, and the six values it deliberately leaves

`v3-web` @ `b10646fc`, merged into `v3`. `cargo test -p roost-client-core
-p roost-web-terminal --no-fail-fast`, **two agreeing runs: 47 binaries, 320
passed, 0 failed, 0 ignored.** `wasm32-unknown-unknown` clean, clippy `-D
warnings` 0, `xtask fmt` clean.

**The 51 errors were not a `cfg` gap, and knowing why changed the fix.** The
sibling `impl` files had been written against a *different web-sys than the
lockfile pins*: 0.3.106 has no `Element::style` (it is on `HtmlElement`),
`get_bounding_client_rect` returns `DomRect` and not `Result`,
`Document::create_text_node` returns `Text` and not `Result`, `HtmlCollection`
has no `get`. The fix was therefore a new seam, not a gate — `element_style.rs`
owning the two properties the stable surface does not expose. And inside it, a
choice worth keeping: **`scrollTop` is read and written as a `double` via
`js_sys::Reflect`, because the `i32` accessor rounds a reader that moved half a
row to *unmoved*** — which is exactly the state the follow-band predicate and
the owned-write check are asked about. A rounding accessor would make those two
predicates unanswerable.

**All four web mutation rows bit**, against a gate doc that recorded one
historically did not. Two results inside that: **M-U1 bit wider than
pre-registered** (9 tests, not the predicted one), which means the edit sits
under more behaviour than the row's author knew; and **M-U3's control was
passing for the wrong reason** — green because the defect under test had
inverted its own guard, so it proved the control worked and proved nothing
about the property. Only running the row tells you which of the two you have.

**Six raw values remain in `sidebar.css`, and the baseline now says six.** v2
baselined this same file at **35** and never drove it to zero, so this is
inherited debt and the number is going down (35 → 9 → 6). Three mapped with no
judgement at all; the remaining six are a U-3 design decision and are listed
here so the next person is not rediscovering them:

| Line | Selector | Value | Why it is not a lookup |
|---|---|---|---|
| 432 | `.mobile-deck-count` | `13px` | ramp has 12 and 14, no 13. The sibling `--fraction` block already uses `var(--md-label-m-size)`, so it reads as an off-ramp one-off — but 13 → 12 or 14 changes the badge |
| 604 | `.terminal-card-title` | `13px` | same off-ramp, one step |
| 673 | `.terminal-card-preview-glyph` | `40px` | `--md-display-s-size` is 36px; the 40px in the tree is a `-line` token, not a size |
| 384 | `.roost-zzz-1` | `6px` | decorative floating badge digit, deliberately below the ramp floor of 11px |
| 385 | `.roost-zzz-2` | `8px` | same; 8px exists only as `--md-space-2`, which would be a spacing token doing a type job |
| 659 | `.terminal-card-preview-text` | `7px` | a monospace preview scaled to fit a 28px card |
The last three would need **ramp steps added**, which is designing. A baseline
set at six is the ratchet doing its job — hold the line here, drive it down from
here — and a baseline set at nine without the migration would have been the
opposite. The distinction is not the number, it is whether the number came down
first.

### Coordinator: `v3-coord` @ `81e492b6`

`cargo test -p roost-coord -p roost-host --no-fail-fast`, **two agreeing runs:
728 passed / 0 failed / 0 ignored**, both times. `roost_host --test spa_path`
7/7; `middleware_spa` 7/7; `diagnostics_rpc` 9/9. Clippy and `xtask fmt` were
re-run after the last edits and their results are reported, not predicted.

**The ratchet moved 31 → 27 `AwaitingDomainPort`**, each row flipped in the same
commit whose `service_impl.rs` arm calls real code: `AuthCoordIdentity`,
`MiscMetrics`, `AuditList`, `DiagDebugLogBatch`.

**`mcp_relays_authority::a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout` PASSES.** That is the test this whole programme held open from its first measurement, and it is settled by a run rather than by an argument. The cause was a race the reading had missed: the deadline arm already answered `Unavailable`, but the pool's own `acquire_timeout` is the *same* 5 s as `STORE_DEADLINE`, so `sqlx::Error::PoolTimedOut` won the race into the blanket `Internal` mapping. "A connection that was not free is not a statement that failed."

**And the propagation was deliberately refused.** Five other sites map `PoolTimedOut → Internal` — `push/rpc.rs`, `ui_state/fence.rs`, `workers/rpc.rs`, `sessions/tasks.rs`, `rpc_transcription.rs` — and v2 has no pool and reports a busy database as `Internal`. Propagating would have been a parity regression on five methods to make one uniform, i.e. optimising for a shape rather than for a behaviour. **The mapping belongs in one shared helper that takes the domain's declared answer**, so it cannot drift, without silently rewriting five of them.

**Four defects the verification found, and three of them are the reason it was worth running.** One is a live production bug on the front door:

- **`middleware/security.rs` overwrote `Vary`.** The CORS layer runs *outside* the SPA, so it sets the header *last*, and its `insert` dropped `accept-encoding` — the token that says a bundle's two answers differ. A shared cache would have handed a gzipped body to a client that refused gzip. Fixed by merging (`append_vary`) rather than replacing, and the SPA test now asserts BOTH tokens survive so neither layer can regress alone. **The generalisable part: a layer that touches a response header is a candidate for this class whenever the composition order changes, and the order is what makes it possible.**
- **`middleware_spa` used `#[tokio::test]`** whose current-thread runtime starves the task `axum::serve` is spawned onto, and the fixture's `get` is a *blocking* socket read. All seven failed on a 10 s read timeout and **none of them were about the SPA**. Any new listener-backed test binary needs `flavor = "multi_thread"`.
- The export sweep counted only its surplus removals, so a boot sweep emptied the directory and reported zero.
- `roost-host` forbids the literal `v2` anywhere in the crate, and the citations broke its install-identity test.

**Three of the lead's own assertions were wrong about correct code**, and one of those is the finding: it compared two reads of a running clock, which is a coin flip on a loaded machine. **A flaky test is worse than no test** — it spends the reader's trust and returns nothing. Delete it or bound it; do not re-run until green and call that a pass.

`xtask lint` reports 2 violations, both in `crates/roost-keeper/tests/`, and both are `v3`'s rather than this track's: the worker branch has carried the restated keeper lint table and all seven binary-level allows since `84be2a9d`, and they clear when that branch merges. The gate number is 2 pending a queued merge, not 2 with a caveat.

**The permanent backstop, and why the direction it checks is the one that matters.**
`crates/roost-coord/tests/method_route_implementation.rs` checks the method
table against the **IMPL**, where `method_route_coverage.rs` checks it against
the **PROTO**. The proto direction cannot lie silently; the impl direction can: a
row marked `Implemented` whose arm is still `delegated_reply` compiles, passes
every other test in the tree, and tells a reader the method works.

**It found one on its first run, and the shape of the symptom is the argument for
it.** `Sync` was marked `Implemented` with no `fn sync` arm — correct, not a
defect: the retired Connect Sync is answered by a *mounted route* returning 410
before `ConnectRpcService` opens a stream, because a throwing stub would keep the
runtime's abort-listener crash path reachable. So a row and its arm coming apart
produces **not a compile error but a 410 that reads like a routing bug**, which
sends the next person to the router instead of to the table. That is the worst
shape a defect can have, and it is invisible to every other check in the tree.

Four guards make the exception set survive contact: `TRANSPORT_ANSWERED` holds
`Sync` **and a second test asserts that list is exactly the set of `Implemented`
rows with no arm** (a documented exception with no bound on its number is a
ratchet with the pin removed); all 16 `UnwiredInV2` delegations are asserted
**correct**, so nobody "fixes" one into a handler; the implemented count is
asserted **above 50**, so the main test cannot be satisfied by emptying the
column; and the test asserts that it read the file `CoordinatorService` is
actually implemented in — without that it would prove a table true of a file
nobody uses.
### Worker: first-ever total, `v3-worker` @ `8a85f523`

`cargo test -p roost-worker -p roost-keeper -p roost-term --no-fail-fast`, **one
run: 530 passed / 48 failed / 0 ignored, 83 binaries.** No worker total had ever
been recorded before this. **This is a triage baseline and NOT a gate figure** —
one run is not two, and the tree moved substantially afterwards.

**THE FIGURE ABOVE IS A COMPILE, NOT A GATE, and this entry did not say so
until now.** Every "0 errors, 0 warnings" reported for this track on 2026-09-27
was `cargo check -p roost-worker --all-targets`, which compiles and does not run
clippy lints. The track gate is `cargo clippy -p roost-worker -p roost-keeper
--all-targets -- -D warnings = 0`, and **the first measurement of it found 7
errors** across six files and four slices — one too-many-arguments, two
large-`Err`-variant, two unneeded-`Ok`-with-`?`, one useless conversion, one
`clone` on a `Copy` type. All seven predate every commit made that day, and none
would have surfaced under `cargo check`. The branch carried a compile-clean
reading for hours because the wrong command was quoted as the gate — **including
by the integrator, in this file, before it was corrected.**

That is the general form below, and its fifth instance: **a compile is not a
gate, and the number that reads like one is the dangerous one.** A measurement is
only a measurement of what it measured. This paragraph is the correction, not the
replacement — the 48-failure triage stands, because `cargo test` was genuinely
run for it.

The finding that matters is the composition of the 48: they collapse to a handful
of root causes, and **exactly ONE is a product defect**. A reviewer who reads
"48 failing" and concludes "this port is broken" would be wrong.

**The first decomposition published here was an UNDERCOUNT, and the correction is
the useful part.** It said seventeen failures were one fixture constant and nine
were one fixture shape. The real extent is **sixteen call sites** of the non-UUID
cell stream id — not four — across `session_cell_sink.rs`, `session_cell_emit.rs`
and `session_raw_metadata.rs`, so `next_cell_frame` returned `Unbuildable` and
every later assertion saw `Withheld(BaselineOwed)`; and **every `WorkerFp` literal
in the tree is 63 hex characters rather than 64**, across four files including the
17-test `session_support` fixture. Three binaries also hit an unset `HOME`
independently, which is a property of `WorkerBoot::resolve` and not of any one
fixture. The total was right; the explanation of it was not. A published
decomposition that turns out to undercount is worse than none — it sends the
next reader hunting one bug where there were sixteen.

The lesson generalises past this number: **triage a count to its root causes,
then verify the extent of each cause before you publish it.** A decomposition is a
claim about every row in the count, and it is as wrong as a count is if it
undercounts one of them.

The real one is worth its own paragraph. `host/install.rs`'s `closing_quote`
returned the index PAST the closing quote, so `split_once('=')` produced a
variable name with a leading `"` that matched no key, and **every systemd scrub
took the "nothing on this line is the key" branch and reported `removed: false`**.
A redeemed bootstrap token survived in the unit; a spent force-live-retire
authorisation survived to re-authorise killing every PTY on the next restart.
Twenty-nine lines of span arithmetic, and a credential and an authorisation both
failing open.

`roost-keeper` on the same branch: `cargo test -p roost-keeper --no-fail-fast`,
**two agreeing runs, 23 binaries, 0 failed.**


### Two process rules, and both were bought the same way

**A verbal handoff has no receipt.** A slice's third file went uncommitted for
two hours because the author messaged the lead "the dirty file is yours to
stage", reported to the integrator "WorkerLead2 is staging it", and then treated
it as done. Two accurate reports; between them they created a belief, in both
people, that a handover had happened. **No `git status` catches a file both
parties have agreed is someone else's problem.**

The narrow rule that would have caught it: **when you hand a file to another
agent, name it in a message to the integrator as well, and say explicitly whether
it is committed.** Not "X has it" — "X has it, it is NOT yet committed, and I
have re-pushed a snapshot containing it". The author also had a snapshot and a
message; what was missing was one line saying which of their files the snapshot
did and did not contain.

**And the complementary half, which is the one that generalises: read
`git status` yourself and do not trust a list.** The author named two files
because that is what they believed they had authored. A person listing their own
work lists what they are thinking about. The rule that actually caught it was
the integrator running `git status` and comparing, because **that check does not
depend on the sibling having kept accurate track.** Both halves are needed: the
list is a question to ask, never a source to trust.

**A green test does not check where its subject is called from.** An enrollment
call placed *after* the link dial satisfies every test in its own binary, because
the tests exercise the function rather than its position in the boot — and being
after the dial is the defect the slice existed to fix. The same shape as a
`#[repr]`-less enum whose `name()` indexes an array (reorders silently, every log
line stays plausible) and as a doc comment naming an implementation that does not
exist (compiles, reads as settled, implements nothing).

**All three are the same class: something that typechecks and reads plausibly
while being wrong.** The defences are structural, not vigilance — a test that
pins the coupling, a commit body that states where each call sits in the order, a
comment that says what a type actually is. **So: when a reviewer is about to
accept "it compiles", the question is what position the code is in, not whether
it builds.** Read the order before the green.

### A consistency test cannot catch a joint violation

The boot order is a fixed array, and `StepId` is a `#[repr]`-less enum whose
`name()` indexes it by `step as usize`. A new test was written to pin the
coupling, then **mutated by swapping the two middle `BOOT_ORDER` rows with the
enum untouched** — the exact rename-everything failure.

**The new test passed.** The pre-existing test, which hard-codes the expected
name vector, caught it.

The reason is the finding: **a test that only ever asks "do these two artifacts
agree?" is structurally incapable of catching "both artifacts are wrong in the
same way."** After the swap, `StepId::KeeperAdmission`'s arm still points at
index 1, index 1 now says `coordinator-link`, and the enum and the array agree
perfectly. The new test checked a **consistency** property; the mutation
violated a **correctness** property. And **nothing inside the crate knows the
correct boot order** — the enum has no independent idea what the right order is,
so the only oracle is a human-written expectation.

The new tests are still worth keeping, for the two failures a name vector
genuinely cannot see: a step appended to one artifact and not the other (where
`StepId::ALL.len() != BOOT_ORDER.len()` is a panic in `complete` otherwise), and
two rows claiming the same name, which is what a partial swap that copies one
name over the other looks like. **But they are subordinate to the name vector,
and a mutation that changes the name vector is a mutation changing the only
place the correct order exists** — which is a review checkpoint the reorder
wants, not a cost.

**When a mutation does not fail the test you expected it to fail, that is a
finding about the test, not a failed experiment.** This is the second time in one
day a test believed to be guarding a property turned out to guard a different
one — the other was M-U3, whose control passed *because the defect under test
had inverted its own guard*. The rule that follows: **publish which test
actually bit.** The instinct to quietly swap in the test that failed would have
destroyed the information, and the information is the result.

**And the failure message is the deliverable as much as the test is.** The
pre-existing assertion printed `left: ["identity", "coordinator-link", …]`
against `right: ["identity", "keeper-admission", …]` with the architecture named
in the assertion text: the whole drift in one line. A well-formed assertion
carries more than a paragraph explaining it, and the paragraph is the thing to
override.

**One operational rule from the same run: do not mutate a tree a test is
reading.** The `BOOT_ORDER` file was restored from a copy while `cargo test` was
still running against it. The restore was verified byte-identical with `diff -q`,
which made that instance recoverable — but a test reading a file while you write
it produces a result about neither version. Kill the run, mutate, re-run.

### The rig, not the port: three reds in one track that all pointed at the wrong seam

The worker track produced three failures in one session whose cause was the
fixture or the test rather than the code under test. **The product was correct in
all three**, and in one of them the *first half of the same test was already
correct*.

| The test said | The truth |
|---|---|
| 24 retained bytes | the literal `b"before anything was armed"` is 25 bytes |
| "the bootstrap token is not spendable" | `Fixture::start` seeds an issued token with an **empty** binding meaning nobody has spent it, and `redeem` fell through to "spent by somebody else" — a **three-state** thing collapsed into two |
| `build_sha(&MapEnv::new()) == None`, asserted unconditionally | the constant is derived from git **at compile time**, so whether it is `None` is a property of the **tree**, not the test. `left: Some("d7675361…")` reads as a build-identity defect and is not one |

The third is the sharpest, because the `match` two lines above the failing
assertion already branched on that same constant and handled both cases
correctly. **Only the closing assertion ignored it.** `build_identity` preferring
the compiled stamp is correct, and a compiled build having an answer with no
environment supplied is not a leak.

**The cost is not the reds. It is that a reader who works out that the cause is
the rig stops trusting the file** — and the next real defect in it goes unread
for the same reason the last one was misread. So each of these is written up in
the commit that fixed it, and the pattern is here for the next track that meets
it.

**The shape to recognise:** *a failure whose message points at the product, where
the message is produced by something that was never under test.* Ask what the
failure is actually **about** before fixing what it appears to be about. Twice
today the cheap explanation was load and twice the serialised or quiet re-run
killed it; twice the plausible cause was the rig.

**And a related habit, from a lead that suspected a sibling's files and was
wrong:** it wrote "almost certainly WorkerStore's, but I am not asserting that
without the name" — the hedge was correct — and then, after measuring, published
the reversal in the same message as its own fix. **A number stays readable
because people correct it in public.** A lead that quietly drops a suspicion
leaves the next reader unable to tell whether it was ever a suspicion.

### CLI cutover, item 1: logrotate landed, and the gate beside it is NOT green

`v3-cli-cutover` @ `fa61f851`, `2L.2`. A rotation plan and its two systemd
units, one `logrotate.d` entry per role, with the second install a
byte-comparison no-op. Ten new tests, **10/10 green in both full-suite runs.**

**The track gate beside it is not met, and the number is worse-looking than a
green one because it is true:**

- run 1: **379 passed / 0 failed**
- run 2: **378 passed / 1 failed** — `dev_fan_out::a_server_that_cannot_start_names_itself_and_stops_what_already_ran`,
  "coordinator never reported handling the signal it was sent", binary time
  0.44 s
- **The two runs do not agree, so this is not a two-agreeing-runs figure.**
- Two isolated re-runs of that target, 4/4 each. The change under test touches
  no `dev/` file, and the flake is load-dependent and pre-existing.
- `clippy -D warnings` and `xtask fmt` were **not run** on this branch for this
  change.

**And one finding worth carrying to Stage 4, because it is v2's answer and not a
gap: v2 installs nothing on macOS.** `apps/coord/scripts/install.sh:601` and
`apps/worker/scripts/install.sh:518` both branch to
`if $IS_LINUX; then write_unit; write_logrotate; …; else write_plist; bootstrap; fi`.
`RotationPlan::Skipped` names `newsyslog` as the thing that rotates there. A
`roost` that installed a `logrotate.d` fragment on macOS would be *less* faithful
than one that does not.

### Keeper client: the plan's premise was stale

The plan recorded three open keeper-client defects on `v3-worker`
(`wait_for_reply` discarding non-matching frames, six missing client frames,
`resize()` discarding the applied seq and geometry). **All three were already
fixed** by commits `94bab2b7` and `be68afa0`, merged in at `8a85f523`, with 14
tests covering them. The plan was reading a doc line the branch had moved past.
The deferral was proved with a mutation rather than an assertion: reverting
`client_frames.rs:121` to the pre-fix drop gives **0 passed / 2 failed**, both
`left: []`.

The lesson is the one this file exists to enforce. A defect list in a plan is a
hypothesis about a tree; a measurement is a fact about one, and the two drift
apart at exactly the rate the tree moves.


## How to read a later gate

- **Phase 2** (Rust worker, TS coord): no spec that passed in the baseline
  may fail. `terminal-delivery.spec.ts:15` ("browser smoke flow creates and
  cleans its resources") is the load-bearing one and must pass.
  **Its ONLY trigger is the worker track being green** — `UNIMPLEMENTED` at
  zero, all nine `SessionManager` collaborators with production impls, the
  composition root wired. A worker that cannot construct a `SessionManager`
  cannot spawn a PTY, and this gate is exactly "can this worker spawn a PTY".
  **The Rust coordinator is not in this gate at all**: the coordinator is the
  TypeScript one, and `ROOST_SMOKE_COORD_EXECUTABLE` is unset. Reading C-B as
  Phase 2's trigger is wrong, and it was written here first and corrected.
- **Phase 3** (Rust coord, then both): no spec that passed in the baseline may
  fail, with `ROOST_SMOKE_COORD_EXECUTABLE` set alone first, then with both.
  **Its trigger is the WHOLE coordinator track green — C-B *and* C-C, which is
  `AwaitingDomainPort` at 0. Not C-B alone.**
  The terminal specs drive the whole session lifecycle through the
  coordinator's Connect API — workspace create, terminal open, spawn, attach,
  input, scrollback, search, attachment, pane close. **Those are the C-C rows**
  (10 sessions, 11 attachments, 2 search, 1 agent prompt, 2 deploy). Without C-C
  the Rust coordinator answers them `Unimplemented`, so **C-B alone produces a
  working link that carries no method handlers, and the run fails on its first
  spec.** C-B is necessary and not sufficient.
  The "both" run needs all of that plus the worker, and the two halves fail
  differently enough that neither check covers the other: the coordinator's
  fails **loudly at startup** (a 401 on every link, no socket at all), while a
  worker that opens a socket and then cannot serve a session fails **silently
  at runtime**. A green coordinator run would not catch the second, and a green
  Phase 2 would not catch the first.
  **This file has now been wrong about the Phase 2 and Phase 3 triggers twice**
  — first crediting C-B with Phase 2, then crediting C-B alone with Phase 3.
  Both corrections are recorded rather than quietly replaced, because the shape
  of the error is the lesson: **a trigger is a claim about what a gate
  exercises, so it has to be read off the specs the gate runs** and not off
  which wave happens to be finishing.
- **Phase 4** is not Playwright — it is
  `crates/roost-client-core/tests/headless_client.rs`, an in-process Rust
  coord + worker that must paint a `MARKER` into a replica viewport.
- **Phase 5** (all-Rust): the full 157 on Chromium and Firefox, plus a
  production build with the `smoke` feature off containing zero occurrences
  of `__smoke` in the bundle.

A gate that cannot run the suite at all has proved nothing. Say so rather
than reporting a partial pass as a green one.

### EXPECTED RED: `a_deferred_reap_waits_for_the_callers_readiness_barrier`

**Read this before the 2C-GATE merge and before the S3.0 workspace run. One test
is red on purpose, and it is not a regression.**

| | |
|---|---|
|**test**|`crates/roost-coord/tests/event_publication.rs::a_deferred_reap_waits_for_the_callers_readiness_barrier`|
|**introduced**|`c02cb9dc` on `v3-coord` — reaches `v3` at the 2C-GATE merge, not before|
|**measured**|`cargo test -p roost-coord --test event_publication` = **5 passed / 1 failed**|
|**green when**|the worker link appends with `defer_snapshot_reap` set **and drains the returned ids** — that is `connection.rs`, in C-B|

**The test asks one question: does anything set `defer_snapshot_reap: true`?** Only
a caller constructing `AppendOptions` to defer can write one. The declaration
(`events/append.rs:274`), the `Debug` field (`:284`) and the read inside
`build_result` (`:367`) are the only other mentions and none can produce a `true`.

**IT MUST NOT BE "FIXED" BY EDITING WHAT IT ASKS.** A softened guard is this defect
with a green suite on top, which is how it was found once already: the first version
of this test grepped for a file mentioning the field without declaring it, and
`append_transaction.rs:78` *builds* the field without declaring it — so **a producer
satisfied a test written for a consumer, six of six green, on a capability with no
consumer.** The replacement exists because a name has both a producer and a
consumer and a grep cannot tell them apart.

**Blast radius while it is red, so nobody escalates it as an outage.** The durable
effective snapshot has already omitted the force-closed ids, so **no route is
resurrected** and no browser sees a stale session. What is lost is prompt cleanup: a
force-closed PTY on an offline worker is never killed and becomes one stale session
row per occurrence, until the worker returns or is deleted. **A coordinator that has
never had an offline force-close has never hit this**, which is why no green suite
ever mentioned it.

**If S3.0 reports this failure, the run is correct and the gate is not met until
`connection.rs` lands. Do not bisect it, do not soften it, do not skip it.**

### The gate ratchets, measured so the exit conditions are concrete

Every track gate has a numeric exit condition. **Measured on `v3` at `93a5f69e`
and on both Stage 3 tracks, so the delta is known before any merge rather than
inferred after one:**

|ratchet|`v3`|`v3-coord`|`v3-worker`|gate requires|
|---|---:|---:|---:|---:|
|`PortStatus::AwaitingDomainPort` rows|27|**27**|27|**0** (2C-GATE)|
|`PortStatus::UnwiredInV2` rows|16|16|16|16, keep them|
|`#[ignore = "UNFINISHED"]`|3|3|3|**0** (2C-GATE)|
|`UNIMPLEMENTED` in `roost-worker/src`|**6**|6|**2**|**0** (2W-GATE)|
|`todo!` / `unimplemented!()` in `roost-coord/src`|0|—|—|0|

**`v3-coord` has flipped ZERO rows, and that is correct** — 1b is an impl, not a
row, and a row flips only in the same commit whose `service_impl.rs` arm calls
the real handler. The ratchet is doing its job by not moving.

**`v3-worker` is at 2 `UNIMPLEMENTED`, down from `v3`'s 6** — `credential.rs`,
`link_wire.rs`, `snapshot_source.rs` and the reconcile block at `runtime/mod.rs:170`
are closed; the two that remain are `runtime/mod.rs:282` (the local door) and
`runtime/link_serve.rs:113` (the hello `capabilities` list).

**AND THAT EXPOSED A CONTRADICTION IN THE PLAN, recorded here so the gate is not
failed for someone else's sequencing error.** 2W-DOOR, the local door, was
scheduled AFTER 2W-GATE — while 2W-GATE requires `UNIMPLEMENTED` = 0 and the door
is one of the two markers that must reach 0. **As written the gate could never
pass, because the work that would make it pass sat behind it.** The door is
therefore part of the composition root and comes BEFORE the gate. A ratchet
cannot distinguish "not done" from "mis-sequenced", and neither could the plan.

### `cargo xtask lint` on `v3` is RED right now, and here is what it is

**Measured at `e694506a`, not assumed.** `cargo xtask lint` on this tree prints
**`xtask: checked 2111 inputs` / `xtask: 10 violations`**, and exits non-zero.

Seven of the ten are `roost-keeper` test binaries that compile the shared
`support` fixture without `#![allow(clippy::unwrap_used, clippy::expect_used)]`
at their root — `tests/channel_history.rs`, `keeper_daemon.rs`, `keeper_dispatch.rs`,
`keeper_endpoint.rs`, `keeper_lifecycle.rs`, `keeper_socket.rs`,
`keeper_socket_protocol.rs`. A crate-level allow is a property of the compilation
unit, so every helper site becomes a clippy error the moment clippy runs.

**TRIAL MERGES MEASURED WHAT EACH TRACK'S MERGE ACTUALLY DOES. All ten of `v3`'s
violations are enumerated here, because an earlier version of this entry named
seven and then inferred the other three — which is the exact mistake this file
exists to prevent.**

`v3` alone: **2111 inputs, 0 unreached, 10 violations.**

|violation|file|
|---|---|
|size, 432 lines against a 400 baseline|`crates/roost-cli/tests/command_tree_shape.rs`|
|size, 409 lines against a 400 baseline|`crates/roost-cli/tests/update_self_replace.rs`|
|lint table: exempt, but its COPY restates only `[unsafe_code]`, so `expect_used`, `missing_debug_implementations`, `rust_2018_idioms`, `todo`, `unimplemented` and `unwrap_used` do not apply to the crate at all — *an exemption is a permission, not a substitute for the table*|`crates/roost-keeper/Cargo.toml`|
|fixture `support` compiled without a crate-level `unwrap_used`/`expect_used` allow, 7 binaries|`roost-keeper/tests/{channel_history, keeper_daemon, keeper_dispatch, keeper_endpoint, keeper_lifecycle, keeper_socket, keeper_socket_protocol}.rs`|

Merging each track into a throwaway worktree, guarded sweep on the result. **The
first attempt used `origin/v3-cli` at `b7d09150`, which was five commits stale —
`v3-cli` had unpushed work. It was pushed and the measurement repeated against
the real ref, and the answer did not change:**

|step|head|conflicts|
|---|---|---:|
|`v3` + `origin/v3-cli@3213f798`|`c2939939`|**0**|
|… + `origin/v3-worker@e6a1e8b0`|`3258073e`|**0**|

**`v3` + `v3-cli` + `v3-worker` = `xtask: checked 2425 inputs`, 0 unreached, 0
violations.** Canary-guarded: the sweep fails loudly if `xtask` did not print its
`checked N inputs` line, so this zero is a measurement and not a failed run.

**AND THE FIVE COMMITS ALMOST DID NOT SURVIVE.** `v3-cli` held 5 unpushed commits
— two `roost-host` commits putting the XDG config and state roots behind a public
API, two docs, and a merge — plus 144 uncommitted lines in
`roost-cli/tests/dev_fan_out.rs`, and **the lead whose session owned them is
dead.** Under the plan's snapshot rule the integrator pushed the branch
(`b7d09150..3213f798`) and the uncommitted work to `refs/heads/v3-cli-snap`
(`043a7961`). **Both are on the remote; the working tree was left intact.**

**That near-miss is why the sweep has a canary and the merges are trial runs.**
Work that exists in one worktree owned by a finished session is one `git gc`
away from gone, and nothing in the build or the tests reports it.

**An earlier version of this entry had the attribution backwards twice.** It
said the size violations were "brought in" by the worker merge — they are `v3`'s
own, and the CLI merge is what clears them. And it named `v3-cli` as the fix for
them while that was still an inference, which happened to be correct and was
recorded as though it were known. **Both errors came from reasoning about merges
instead of running them**, and one command in a throwaway worktree answers the
question in under two seconds. The scratch worktrees were removed; neither
measurement touched `v3`.

The new unreached-module rule contributes 0 of these.** Verified by stashing it:
10 violations before, 10 after, with inputs checked going 1480 → 2111.

### What Phase 6.4 costs: run the classifier, do not read a table

**There is deliberately no count in this section.** It was hand-maintained and corrected
three times — 155 lines, then 70 and 8, then 67 and 12 — because every correction was a
stale number and each cost a turn. **6.4 runs the classifier below against its own tree;
a table written today describes a tree that will not exist on 6.4.** What survives is the
part that does not go stale:

- **The `L11` / `lint-roost.ts` guards go dead with the TS job.** 6.4 deletes the script
  *and* the TS invariants CI job, so every guard citing one loses its enforcement. These
  need re-expressing in `xtask lint` — a different kind of change from a test, and the
  only bucket here that is not mechanical.
- **Two entries have no guard at all**, which the index's own rule forbids: `A defaulted
  injectable host function loses its receiver` and `Roost never owns the agent session`.
  The first explains why — "Bun unit tests pass either way; only the live/Playwright
  browser pass exercises the receiver" — which is a real reason and not an excuse, but a
  reason is not a guard. **Neither is findable by a grep over the tree, so 6.4 would pass
  them by default.** They need a test written, not a path edited.
- **`smoke/` guards are safe.** The plan kept smoke TypeScript as the oracle rather than
  porting it, so those guards keep working untouched. This is the one place the TS
  deletion is a benefit, and it is worth remembering as one.
- **Per-entry counting is required and a line count is not a substitute.** Counting lines
  across the file also matches `**Wrong**`/`**Right**` prose and double-counts entries
  naming two paths. Both happened; the second produced a bucket summing to 105 against 102
  entries.

```bash
python3 - <<'PYEOF'
import re
txt = open('docs/FAILURE-INDEX.md').read()
blocks = re.split(r'^### ', txt, flags=re.M)[1:]
DELETED = ['apps/roost-cli/', 'apps/worker/', 'apps/coord/', 'apps/web/', 'packages/']
b = {k: [] for k in ('repoint', 'lint', 'smoke', 'rust', 'noguard')}
for blk in blocks:
    head = blk.split('\n', 1)[0].strip()
    m = re.search(r'\*\*Guard\*\*\s*(.*?)(?=\n#{2,3} |\Z)', blk, re.S)
    g = ' '.join(m.group(1).split()) if m else ''
    cites_lint = ('lint-roost' in g) or ('L11' in g)
    hits = [p for p in DELETED if re.search(re.escape(p) + r'[A-Za-z0-9_./-]*', g)]
    # a packages/ mention beside a Rust test AND a "was" marker is HISTORY, not a live ref
    hist = ('packages/' in hits and re.search(r'\(?\bwas\b', g)
            and re.search(r'crates/[a-z-]+/tests/[A-Za-z0-9_./-]+', g))
    if hits and not hist:            b['repoint'].append(head)
    elif cites_lint:                 b['lint'].append(head)
    elif 'smoke/' in g:              b['smoke'].append(head)
    elif re.search(r'crates/[a-z-]+/tests/', g): b['rust'].append(head)
    else:                            b['noguard'].append(head)
total = sum(len(v) for v in b.values())
for k, v in b.items():
    print(f'{k:<9} {len(v):>4}')
print(f'{"PARTITION":<9} {total:>4} of {len(blocks)} entries')
assert total == len(blocks), 'BUCKETS DO NOT PARTITION THE INDEX'
print(f'touched by 6.4: {len(b["repoint"]) + len(b["lint"]) + len(b["noguard"])}')
PYEOF
```

The `assert` is the part that matters. It is the check both hand-written versions
lacked, and it is what caught them.

### The import check RECOMPUTES its expected counts, and restates the filter

The Phase 6 install gate asserts row counts after `roost import-v2`. **It must
not assert hardcoded numbers, and the reason is specific.**

Measured once, from `coord_v2.2026-09-26T13-32-32-235.db.gz` (38M gz, 391M
inflated, `integrity_check` `ok`, sha256 `13becff426a0d3f…cb740`): accounts 1,
`account_devices` 26, `authorized_keys` 31 with **26** in the imported set,
`authorized_key_revocations` 241, `app_settings` 7, and one row each in
`organizations`, `organization_memberships`, `account_identities`, `dashboards`,
`dashboard_memberships`.

**THOSE NUMBERS ARE NOT THE GATE, because that file does not last.**
`RoostCoordinatorV2/backups/` keeps a rolling 14 (oldest today `2026-09-11`),
so this one is rotated out around 2026-10-11 and the gate may well run after
that. A hardcoded 26/26/241/7 then fails on a difference that is **correct v2
state** — a device paired or a key revoked since — and the operator spends the
debugging session on the importer instead of on the thing that changed.

**SO THE GATE RECOMPUTES from whichever backup it inflates, by its own SQL, and
prints that file's sha256 so a run is attributable.** The count is not the point;
the *filter* is, and the filter cannot be recomputed by calling the importer:

```sql
-- restated here on purpose: 26 exists ONLY because this predicate runs
select count(distinct k.fingerprint) from authorized_keys k
 where exists (select 1 from account_devices d where d.fingerprint = k.fingerprint);  -- 26
select count(*) from authorized_keys k
 where not exists (select 1 from account_devices d where d.fingerprint = k.fingerprint); -- 5 machine keys
```

**Borrowing the importer's own filter would make the gate assert the filter
against itself** — it would pass with the filter deleted. The two `exists`
clauses above are deliberately written out, and this SQL was run independently
to confirm it reproduces 26 and 5. Everything else is an identity assertion
(imported rows == source rows for that table) and needs no restating.

### Two more, both measured rather than argued

**The worker track's first clippy measurement is 0, at `e6a1e8b0`.** It is the
**first** — dated, and explicitly not a continuation of a number that was never
measured. The command was `cargo clippy -p roost-worker -p roost-keeper
--all-targets -- -D warnings`, run with `git status --porcelain` empty in the same
shell so the figure belongs to that tree and not the one before it. Seven errors
were found and fixed first; **three of them, and three of the worker's
contribution, would not have surfaced under `cargo check` at all.**

**Three capabilities on that track are unreachable, and the reachability filter is
what found it — not a review and not a test.** `grep -rn 'WorkerCapabilities'
crates/roost-worker/src` returns three hits: a comment, the definition, and the
impl. **Nothing constructs it.** So `SessionManager` cannot be built, the browser
link runs `BrowserLink::detached()`, and three of the nine collaborators have no
production implementation. The same filter found the lead's own `cell_row_json`
nine seconds after a careful read of the diff had missed it.

**That is the argument for making reachability a gate rather than an audit.** A
name-count sweep is not a conclusion — it produces candidates, and a human judges
them. But the one candidate that mattered was invisible to both a diff review and
a passing test suite, and the filter that found it is one `grep`.

**THE RULE THIS PRODUCES, and it is the only one here that closes the class
rather than the instance.** Seven capabilities have now been found unreachable by
one instrument — `adopt_survivor`, `WorkerCapabilities`, `cell_row_json`, the
`LiveEffects` pair, the coordinator's `ClientSeqCursor`, the whole deferred-append
path including its flag, its store field, its claim hand-back and two green tests,
and `WorkerRouteIndex::bind`. In every case the instrument was a `grep` for the type
or the field, and in every case a review, a diff read, and a passing suite had all
missed it.

**THE RULE NOW EXISTS AS CODE, and it found a real file on its first honest run.**
`xtask/src/unreached_module.rs`, wired into `cargo xtask lint`: every `.rs` file
under a crate's `src/` must be reachable from that crate's module roots through
`mod` declarations. The graph is transitive, because a file declared by a module
nothing reaches is itself unreachable and naming the child points at the wrong
file. It is deliberately NOT a dead-code detector — it answers "is this file in
the module graph", not "is anything in it called", and a rule trying to be both
would produce false positives and get deleted.

Swept across all five tracks with `ROOST_REPO_ROOT`: **`v3`, `v3-coord`,
`v3-worker`, `v3-cli` and `v3-cli2` each report 0; `v3-web` reports 8.** Seven
are `find`/`backfill`, queued for registration. The eighth is new and nobody had
named it: **`roost-web/src/platform/peer.rs`, 365 lines of ported WebRTC, declared
by nothing** — and its own header claims *"reached by reflection because the
WebRTC `web-sys` features are not enabled."* That sentence is an eighth instance
of this class, not a defence against it: a comment asserting a reachability the
build does not have, over code no `cargo test` has type-checked.

**NO BASELINE WAS TAKEN, deliberately.** Every one of the eight has a named fix
in flight. The size and console ratchets exist for pre-existing debt nobody is
addressing; baselining work that is queued would make the tree read clean while
eight files stay uncompiled, which is the failure this whole class is about.

**AND THE INSTRUMENT HAS A TRAP THAT PRODUCES THE MOST DANGEROUS NUMBER HERE.**
`xtask lint` shells out to `cargo metadata`. Run the binary without `cargo` on
`PATH` and it exits 1 printing one line, and a `grep -c` over that output returns
**0 — indistinguishable from a clean tree.** That mistake was made twice while
measuring this rule, and it is the same class as every other finding tonight: an
instrument reporting success while doing nothing. A zero from a tool that failed
is worse than an error, because nothing prompts anyone to look again.

**SO THE SWEEP GUARDS ITS OWN OUTPUT, and runs a known-positive tree first.** A
count with no canary cannot distinguish "nothing found" from "nothing looked at":

```bash
export PATH="$HOME/.cargo/bin:$PATH"
BIN=/home/almalinux/repos/roost-v3/target-gate/debug/xtask
sweep() {
  OUT=$(ROOST_REPO_ROOT="$1" "$BIN" lint 2>&1)
  # a run that did not print this line FAILED; its zero is not a result
  if ! printf '%s' "$OUT" | grep -q '^xtask: checked'; then
    printf "%-18s TOOL FAILED: %s\n" "$2" "$(printf '%s' "$OUT" | head -1)"; return
  fi
  printf "%-18s unreached %2s   (%s)\n" "$2" \
    "$(printf '%s' "$OUT" | grep -c 'not reachable from any crate root')" \
    "$(printf '%s' "$OUT" | grep '^xtask: checked')"
}
# CANARY FIRST — v3-web must print 8. If it prints 0, every other number is fiction.
sweep /home/almalinux/repos/roost-v3-web v3-web
for w in roost-v3 roost-v3-coord roost-v3-worker roost-v3-cli roost-v3-cli2; do
  sweep "/home/almalinux/repos/$w" "$w"
done
```

**The verified sweep, with the input count beside every figure so a zero is
never bare:** `v3` 0 of 2111, `v3-coord` 0 of 1607, `v3-worker` 0 of 2414,
`v3-web` **8** of 2363, `v3-cli` 0 of 2124, `v3-cli2` 0 of 2130. Five trees read
zero because five trees were examined.

**THE COUNT WILL KEEP MOVING, so do not cite it — grep, and add to the list.** A
number in this file is a number from the day it was written, and the day this class
grew from six to seven the only thing that changed was that someone ran the filter
again. The list is the durable part; the tally is not.

**AND THE SEVENTH INVERTS THE PATTERN, which is why it is the one worth having.**
The first six are code nobody calls — dead weight. `WorkerRouteIndex::bind` is a call
nobody has made *yet*: it is a method the tree **needed and did not have**, so its
absence was going to force a new public API invented to paper over it. **That is a
stronger argument for the filter than the other six, because it is not only finding
code nobody calls — it is finding the calls nobody has made yet, and the gap between
those two is exactly where a new interface gets invented.** A filter that only found
dead weight would be a cleanup tool; this one finds the seams the next commit needs.

**So: assert reachability, in the tests that already cover the capability.** A
test that proves a value is right is not the same claim as a test that proves
something asks for it. The second is the one that catches this class, it costs one
assertion, and it belongs in the existing test rather than a new file — because the
test is where the camouflage already is.

**And the instance that produced the rule, because it is the first whose guard is
a PASSING TEST rather than a comment — and that inverts the usual expectation.**

**And a fifth instance of the pattern below, which is the first where the guard is
a passing test rather than a comment.** `grep snapshot_reap_ids` outside
`events/append.rs` returns six hits and **not one production reader**: the field
declared, cloned, stored, and dropped. `ClaimOutcome::Claimed` genuinely hands the
stored effect back *including* the ids, and the caller copies them into the
result and returns. So the store is not the missing piece — **the drain after the
claim is.** The `kill_orphan_pty` call site is guarded by
`!options.defer_snapshot_reap`, so on exactly the path where a worker connection
is involved the method is never reached.

The two green tests are the sharpest part: **they assert the ids come back
correctly, and nothing consumes them.** A force-closed PTY on an offline worker is
never killed and becomes a session row that outlives its process, one per offline
force-close. The route is not resurrected — the durable snapshot already omitted it
— so this is prompt cleanup lost, and the shape of the loss is the whole session's
theme: **a green suite is the best camouflage a silent drop can get, because it
converts a defect into an apparent success.**

### An instrument that reports success while doing nothing

**This is the through-line of the whole 2026-09-27 session, and it was found four times before anyone named it.** Each instance looks like it is doing its job, and in three of the four there was a passing gate behind it.

| The instrument | What it reported | What it did |
|---|---|---|
| `middleware/security.rs` overwriting `Vary` | a response with headers | dropped `accept-encoding`, so a shared cache could hand a gzipped body to a client that refused gzip |
| `CoordTerminal`'s `Debug` | two collaborator type names | `type_name::<Self>()` for both fields — it named the container twice, under a comment saying the point was to name which were wired |
| `SocketClose::Default` | a close code | none, and the absence *was* the instruction: a durable append that threw means reconnect and replay, and any code tells the worker to back off instead |
| `LiveEffects` | — | prevented by its own doc, *"no default bodies, because a default that silently does nothing is exactly the history-corrupting drop this subsystem exists to prevent"* |

**The common shape is a component whose output is indistinguishable from its output when the work is absent.** A missing header, a duplicated type name, an absent close code, a defaulted effect: in each case the failure appears somewhere *else* — a cache serves the wrong bytes, a reader concludes a seam is reporting, a worker backs off, a PTY is never killed. Nothing points back.

**The defences, and they are not the same defence in each case.** A merge rather than an `insert`; a name captured at the wiring site so it cannot derive from `Self`; a test that asserts the *absence* is load-bearing; a doc that forbids defaults. Three of those four are structural and one is a comment, and **the comment is the weak one** — which is why the `LiveEffects` methods want tests as well as the doc.

**And the general question, which is the same one this file keeps asking in a different costume: what could this check have detected before you consumed what it returned?** A gate that cannot see the thing prints the same verdict as one that can. That is now five instances here — this one, the unregistered module, the unregistered `WorkerCapabilities`, the `flock` on a missing directory, and the compile quoted as a gate.

### A count that cannot tell "added" from "moved" is not a count of additions

**The sixth instrument of the night, and the only one that made a false ACCUSATION
rather than a false pass.** A commit reported as *"33 insertions, 15 deletions,
all whitespace and line-wrapping"* was checked with two instruments and declared
to contain new test code:

- `git diff | grep '^\+'` — **prints only added lines, so a reflowed assertion
  looks like a new one.** Wrapping `assert_eq!(SocketClose::QueueOverflow.code(),
  Some(CLOSE_QUEUE_OVERFLOW));` into four lines puts the name on a `+` line
  although nothing was added.
- `git show $c -- <file> | grep -c CLOSE_QUEUE_OVERFLOW` — **`git show` prints
  `-` lines too, so a reflow counts exactly like an addition.** It returned 4.

**The report was true. The commit was pure formatting, and the arithmetic agrees:
three one-line `assert_eq!`s wrapped to four give +12/−3, one `assert!` gives
+4/−1, the `use` block expanding gives +3/−1, and the import line about +2/−0 —
roughly +21/−5, which is the stat exactly.**

**What settles it, and it costs one command:**

```bash
git diff -w --word-diff <a> <b> -- <file>   # empty or whitespace-only => a reflow
git show <rev>:<file> | tr '\n' ' ' | tr -s ' ' | grep -o '<pattern>' | wc -l
```

The second counts ASSERTION STATEMENTS rather than lines mentioning one, so both
commits answer 5 close-code and 3 keepalive — identical, and nothing was added.

**The general rule, and it is the same as every other entry here: an instrument
has to be able to fail before its number means anything.** `--stat` counts lines,
`grep` counts mentions, and both read a moved line as a new one. **A measurement
that cannot distinguish two cases cannot support a conclusion drawn across them**,
and the tell is that two instruments agreed — which felt like confirmation and was
in fact the same blindness twice, pointing the same way because they were the
same kind of instrument.

### A green check on a file nothing includes is not a check

A draft put a struct field inside an `impl` block. Rust reports that as an
error, **but only if it looks at the file** — and it did not, because the
module had not been registered in `mod.rs`. So a `cargo check` run against the
tree came back **clean** while the file being written could not possibly
compile. The lead deleted the file rather than push it, and the finding is
recorded because the shape recurs: **a check that returns zero because the
compiler was never pointed at the thing you changed is not evidence about
anything.**

The rule: **register the module in the same edit that writes it.** If a file has
to be written in stages, the stage boundary is where the `mod` line goes — not at
the end, and not in a follow-up. A sibling crate in this programme has the same
exposure in a different form: a test binary that compiles a shared fixture
without declaring its allow *at its root* was invisible to clippy for the same
reason, and the lint only saw it once the binary was on the list.


**The second instance was found by probing rather than by reading, and the first
draft of this entry had both of its facts backwards.** A build directory was
removed out from under a track, and the command was
`flock <dir>/.roost-build.lock cargo check … 2>&1 | grep -E '^error'`. It came
back in **0.22 s with no output** on a crate whose check takes 50 seconds, and
was read as clean. What actually happened, established by probing `flock`
directly:

1. **`flock` CREATES the lock file it is given.** With the lock file moved away,
   `flock <dir>/.roost-build.lock true` exits 0 and recreates it. **So the
   existence of the lock file proves nothing** — it is created on first use, not
   found. A guard that tests whether the lock is present is testing something
   `flock` manufactures.
2. **A missing parent DIRECTORY makes `flock` fail**, with exit 66 and
   `cannot open lock file`. So the guard this file would naturally carry —
   *"if `flock` fails, stop"* — **did** have something to fire on, and `flock`
   **did** fail correctly. It was not defeated by `flock`; it was defeated by
   nobody reading the exit status.
3. **The pipeline then hid the failure twice over.** `grep -E '^error'` matched
   nothing, so it exited 1 — but what was read was the *output*, which was empty,
   and empty was read as clean. And the structural hazard runs the other way
   too: without `pipefail` a pipeline's status is the **last** command's, so a
   cargo failure that `grep` *does* match on becomes a pipeline exit of 0. A
   green exit from `| grep` can mean the build failed and was filtered into
   nothing.

So the rules, corrected: **a lock that is CREATED rather than FOUND proves
nothing about what is behind it**, so assert the directory; **`set -o pipefail`
so a real failure is not masked by a filter that swallowed it**; and **check the
exit status, not only the output**, because the most dangerous thing a failed
build produces is no output at all.

```sh
set -o pipefail
export CARGO_TARGET_DIR=<worktree>/target-track
test -d "$CARGO_TARGET_DIR" || { echo "MISSING TARGET DIR" >&2; exit 1; }
flock "$CARGO_TARGET_DIR/.roost-build.lock" cargo "$@"
```

`test -d "$CARGO_TARGET_DIR"` and not `test -d <path>`: the hazard is specifically
that the variable names a path `cargo` will silently ignore, so the thing to
assert is the **variable's value**, which cannot pass on a typo between the
brief's path and the worktree's.

**The four silences, which are one class and not four small mistakes.** A filter
that matches nothing; a package that no longer exists; a `--test` that names no
target (which prints a *suggestion*, and reads past easily); and a missing target
directory. Each was met separately and filed as its own incident. **Silence is
the only output all four share**, and grouping them is what makes `test -d` a
rule rather than a patch for one evening.

### A commented-out `pub mod` is a silent un-registration, and the rule for it is NOT a marker list

A subagent disabled three module registrations in a row with
`// TEMP-DISABLED pub mod predictive_echo;`, then `// TEMP pub mod
attachments;`. Each one **removes a module without failing where the edit was
made**: the files stay, the crate still compiles, and the failure surfaces in a
different crate as `could not find predictive_echo in client` — four files from
the cause, phrased as a missing module rather than a commented-out line. The
reader goes looking for a file that is present. It is the same class as a green
check that never ran, and it cost a lead three restores and a build cycle.

**The marker names are unbounded, so a lint listing them is not the fix.** A rule
naming `TEMP`, `FIXME`, `XXX` and `for now` passes on the next spelling. And the
obvious shape test — a `mod` or `use` with a `;` after it — **flags real prose in
this repository**: `// happens to declare \`mod api_support;\`.` quotes a
declaration and disables nothing, and `// this use of the term is deliberate` has
a `use` in it. A lint that cries wolf on the first file it reads is worse than
no lint, because the next occurrence is ignored.

**So the rule was written, made its own tests pass, flagged prose, and was
deleted.** A gate that is broken is worse than a gate that is absent, and an
optional rule is not worth a broken `xtask`. What survives is the rule written
here rather than in code:

- **A module is registered, or it is not in the tree. There is no third state,
  and no marker that temporarily un-registers one.** An unregistered file is
  **never compiled** — it is unchecked text, which is exactly the "green check on
  a file nothing includes" case in the section above. Every type drift in it
  stays invisible until somebody writes the code that reaches it.
- So a module written and not yet reachable is **registered anyway, and reported
  as "declared, no adapter yet."** A trait with no implementation compiles fine
  once registered — at worst a `dead_code` warning, which is allowable per item
  with a reason. Unregistered, it is not checked against the crate's types at
  all, which is strictly worse than a warning.
- If a slice needs a registration it does not own, it says so in its report and
  the integrator writes the line. A slice that cannot compile without a `mod` it
  does not own is reporting a dependency, not working around one.
- Whoever eventually writes the lint should match a bare `mod`/`use` token within
  the first few words of a `//` comment **and** a `;` on that line, and should
  carry the two prose lines above as named regression tests, because a version
  that flagged them would be reverted by the first person who met it.

**The backstop, scoped so it does not become a superstition: a check faster than
the crate has ever checked has not checked.** It bites on `cargo check` and
`cargo test`, and explicitly **not** on a source-tree scan — `cargo xtask lint`
walks files and counts lines, so 0.62 s for it is genuinely fast and does mean
what it says. A rule that fires on every fast result trains people to ignore it.

**A gate that reports nothing is not a gate that passed.**

**The general form, and it is the fourth instance today:** an instrument that
cannot see the thing reports success. A sweep that counts only its surplus
removals reports zero for a sweep that emptied the directory. A rate window that
matches nothing is zero violations. A `SpawnNotAcknowledged` under load reads as
a refused spawn. **Ask what the check could have detected before consuming what
it returned** — that question has caught more real defects today than any
individual test.

### A SQLite digest is only comparable after every connection is closed

**The rule: anything that compares a database file byte for byte must close
every connection to it first, and the comparison must say when it took its
snapshot.**

SQLite checkpoints its WAL and writes the main file when the **last connection
to a WAL database closes** — not when a write happens. So a database can be
byte-identical at the moment a function returns and different a moment later,
with no writer in between. An assertion that hashes the file inside the function
that used it is a race with a timer on it, and it fails intermittently in a way
that reads as corruption.

This is not only a test artefact and it was not found by a test. **`roost
import-v2 --dry-run` has to answer "what would change" about a target
coordinator database, and Stage 3.3's install gate and Stage 4's cutover both
compare digests across a live coordinator.** A false difference there reads as
"the import corrupted the database", which is the one conclusion an operator
cannot afford to draw and cannot easily disprove. So:

- Close the pool explicitly before any caller reads the file, and say so in the
  assertion's own text — an assertion that does not say when it snapshots is the
  defect.
- **And a digest is not the right instrument for a live database anyway.** A
  count of the rows that matter is what proves the import did its job; the digest
  is what proves nothing moved, and for that the file must be closed first.

The related discovery, from the same work: `roost_coord::db::open` **runs the
migrations**, so a command that promises not to touch a database must not reach
it through that function even to look. `--dry-run` asking a read-only handle
whether the target has an `accounts` table, and reporting a first run when it
does not, is the shape that keeps the promise.

### Two more, both of which cost a whole agent-hour to learn

**A diagnosis that survives only the fix it proposed is not a diagnosis.** A
worker test failed on a 5 s `SpawnNotAcknowledged`, which reads exactly like the
load story above, so the explanation was load. It died on a **serialised** re-run
— same result, no other track building — and what was underneath was
`KeeperFixture::start`: a strict accept/serve loop that serves exactly ONE
connection at a time. A test holding the first pool alive across a second
connect makes that second `connect` retry to the timeout, and the failure it
produces **names a SPAWN rather than a connect**. So the rig lied about which
seam it broke, and the plausible cause pointed at the product.

The rule is not "be more careful". It is: **if your explanation is load, and the
re-run under quiet conditions reproduces it, your explanation was wrong.** Load
is the cheapest available explanation and it is the one most likely to be
assumed rather than eliminated.

**A test red for a reason unrelated to what it tests trains a reader to ignore a
red in that file.** Two of this track's reds were literal-versus-assertion, not
logic: one asserted 24 for a 25-byte literal, another named a spawn when the
rig had broken a connect. Neither is expensive to fix, and both are expensive to
leave, because the cost is not the wrong assertion — it is that the file stops
being read. **When a test fails, check what the failure is actually about before
fixing what it appears to be about.**

**And the one that is a process rule rather than a testing rule: a shared
working directory is not yours.** `git add -A` across a tree three agents were
editing captured a sibling's uncommitted fix in its pre-fix state and silently
reverted their work; neither noticed until a test failed, and the fix existed in
exactly one place for as long as that took. Stage an **explicit path list** and
ping the owner before committing anything you did not write. `-A` cannot
distinguish "I read this and it is right" from "this was on disk", and on a
shared tree the second is most of it. An unstaged file is a five-second fix; a
wrong commit is a red tree for everyone.


### Two ways to misread a red run before you have read it

**A `SpawnNotAcknowledged { timeout: 5s }` under load is LOAD, not a refused
spawn.** `SPAWN_ACK_TIMEOUT` is 5 s and it is not a fixture's to widen. A test
binary that opens REAL PTYs competes with every other thing compiling on the
machine, and the first observed instance came from a real keeper starved by
three concurrent track builds. At a gate, that failure is evidence about the
machine. Reading it as a product-seam defect sends you to debug a keeper that
was never asked a question it could not answer, and the expensive part is the
hours, not the mistake. **If a spawn timeout is the only red, re-run it with
the machine otherwise idle before believing it.**

This is the same shape as the known `terminal-peer.spec.ts:263` deviation: a
spec that passes isolated and fails under full load is a load signal until a
quiet full run says otherwise.

**A compile error in one crate is a COMPILE error, not a verdict on the port.**
The worker track's first recorded total was 48 failures across 83 binaries, and
it would have been easy to read that as a broken port. Twenty-nine lines of
span arithmetic in a systemd scrub were the only product defect in it; the
other 47 were seven fixture-shape problems — a `WorkerFp` that wants 64 hex
where the fixture passed a UUID, a cell stream id that must be a UUID, an unset
`HOME`. **Triage a red count to its root causes before you characterise it.**
A count and a character are different claims, and only one of them is
supported by the number.

**A test that has never RUN is a liability, and a red one is information.** Five
named tests in the worker track were written and never executed, blocked behind
a `String`/`&str` in a fixture. Reporting them as "the slice is written" would
have been a claim about bytes rather than about behaviour, and the gate would
have inherited four of them as unverified. When a figure is unrun, say
`unrun` — an unrun gate item labelled unrun costs nothing, and a claimed one
costs the gate.


## Phase gates

**The section the plan requires, created before the first result rather than
under time pressure.** Every entry carries five things and nothing else, because
a gate record that omits one of them cannot be compared with the next one:

|field|why it is required|
|---|---|
|**tree SHA**|a figure without one belongs to no tree and cannot be re-checked|
|**date**|the same command on the same SHA can differ across days|
|**the exact command**|two runs of "the tests" are not comparable|
|**pass / fail / skip, per run**|the TS baseline's skips are named, so ours must be too|
|**agreement across runs**|one run is an observation; two agreeing runs is a gate|

**THE TS ORACLE, which every figure below is read against:**

|pass|expected|
|---|---|
|correctness (serial)|**142 passed / 0 failed / 3 skipped**|
|perf (`@serial`)|**15 passed / 0 failed / 3 skipped**|

**The six named skips must keep skipping for the same reason.** A skip that
starts skipping for a NEW reason is a regression wearing a skip's clothes, and
it is the one failure mode a pass/fail/skip summary cannot show.

### Gate results

*None recorded yet.* Stage 3 has not run. The three gates, in order:

1. **Phase 2** — `ROOST_SMOKE_WORKER_EXECUTABLE=<release>/roost bun run test:terminal`.
   The load-bearing spec is `terminal-delivery.spec.ts` *"browser smoke flow
   creates and cleans its resources"*.
2. **Phase 3** — the same with `ROOST_SMOKE_COORD_EXECUTABLE` alone, then with both
   variables. **The "both" run is the stack production will run in Stage 4.**
3. **Phase 6 install** — the scratch `roost3gate` user, browser pairing, the
   keeper PID across a deploy, and the import check.

### Where tonight's numbers live, since this file is long

Everything measured on 2026-09-27 while the tracks were running is in the
sections above, and each says which tree it was measured on:

- **EXPECTED RED** — the one deliberately red test, its commit, and its green condition.
- **The gate ratchets** — `AwaitingDomainPort` 27, `UnwiredInV2` 16, `UNFINISHED` 3, worker `UNIMPLEMENTED` 6, `todo!` 0.
- **`xtask lint` on `v3`** — all ten violations enumerated, and what each track's merge clears.
- **The unreached-module rule** — the guarded sweep, its canary, and the eight files it found on `v3-web`.
- **The import check** — why it recomputes its counts and restates the fingerprint filter in its own SQL.
- **The instruments** — four findings whose common shape is a check that could not see the thing it claimed to check.

## Perf numbers worth not regressing

From the baseline's own instrumentation, since the perf specs assert against
their own budgets and a silent 2× drift is easy to miss in a pass count:

| Case | Baseline |
|---|---|
| `history_scroll_plan` | 8 steps, 250-row pager fetch, 500-row ahead |
| `history_scroll_latency` | median 178ms, max 293ms per step, 4 total RPCs, 30s budget |
| navigation → first paint | `cold_driver_to_paint_ms` 914, fresh navigation 3.0–3.7s |
| 20k flood | 306ms wall, 0 dropped phases, 19,970 retained, 259 DOM nodes, 32 cell rows |
| bounded deck (16 spawned) | 9 mounted slots, reveal p50 47ms, p95 57ms |
| peer perf (Sync vs loopback vs direct) | asserted by `terminal-peer-perf.spec.ts:7` |

Retained-marker bounds are worth keeping in view because they are the
history-corruption tripwire: a Rust renderer that drops the retained floor
will pass every functional spec and still lose scrollback.
