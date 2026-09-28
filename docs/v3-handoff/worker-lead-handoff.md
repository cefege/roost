# Track W lead handoff — `v3-worker`

Written by `WorkerLeadW2`. The previous lead (`WorkerLeadW`) stood down mid-wave and
its work is preserved in the tree and in commit `d02d6ec3`; this file supersedes its
own. Everything below is a measurement with its tree named, or a decision with its
reason.

## Where the branch is

- Worktree `/home/almalinux/repos/roost-v3-worker`, branch `v3-worker`.
- `d02d6ec3` — `worker: W-C compile-to-green for the library — test targets still red`
  — is the W-C checkpoint. Pushed. **The library result belongs to this commit.**
- **`d02d6ec3` must not be merged as it stands.** Its `crates/roost-keeper/Cargo.toml`
  hunk breaks the whole workspace (see below). Merge the branch TIP, never that commit
  by name. The corrected manifest is uncommitted on `v3-worker` as of this note and is
  on the integrator's carry row M1 as an amendment.
- Every cargo command needs:

  ```
  export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
    CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-worker/target-track
  ```

  Never `target-gate`, never another track's `target-*`, never a higher
  `CARGO_BUILD_JOBS`.

## The one measurement that matters, and how it was taken

```
cargo check -p roost-worker --all-targets --keep-going
```

- **`roost-worker` lib: 0 errors. `roost-keeper`: compiles.** The five W-C compile
  slices' work holds up under rustc.
- **~56 errors, all of them in `crates/roost-worker/tests/`, across 14 targets.**

`--keep-going` is not a detail. `--all-targets` builds the lib first and stops at the
first failing target, so the test binaries were never reached. The "41 errors" the
previous lead reported was a floor published as a total. These test targets have
never been compiled in this project's history; almost nothing in the 56 is new
breakage. It is missing imports, private-item access, one moved value and two
integer-width mismatches.

**"The worker builds" and "the worker's tests build" are two separate claims here,
and only the first has ever been true.** Expect this on other tracks too.

**And every per-target count is a floor for a SECOND, independent reason:**
unresolved-name errors stop rustc before method resolution, so the E0599s in a target
are never emitted. Two runs will agree with each other and both be wrong in the same
direction. **Agreement is not completeness.** A figure is a total only once the
previous run's errors are actually GONE, not merely stable.

**And a count can be attributed to the wrong file.** `keeper_pool_support/mod.rs`
returned a `MutexGuard` where a `Seen` was declared. That module is compiled into
three separate test targets, so the arbiter attributed the same single defect to
three different files — it was 3 of `WtPool`'s 5 diagnostics, none of them in the
file the count named.

## The lock convoy, and the rule it produced

Four to seven slices sharing one target directory serialise on cargo's directory
lock, so a slice told to "verify with cargo check" is competing for the exact
resource its siblings hold. Two slices died holding correct, complete edits with no
compiler verdict. The rule now in force: **slices edit and report; the lead runs the
compiler, once, for the whole wave, and routes each diagnostic to the owning slice.**
A finished-and-unverified slice beats a slice that spent its run in a queue.

## `roost-keeper`'s lint table is a COPY, and that is the only form cargo accepts

`[lints] workspace = true` together with a `[lints.rust]` table is a hard manifest
error:

> cannot override `workspace.lints` in `lints`, either remove the overrides or
> `lints.workspace = true` and manually specify the lints

`d02d6ec3` shipped the inherit-only form, which broke the WORKSPACE — not just this
crate, because `roost-keeper` is a dependency of `roost-coord` too. The corrected
table restates the workspace values with the one genuine override.

Before that table existed at all the crate was under NO deny table, which is how 19
production `expect()`s survived a sign-off that read as green. The override itself is
narrow: the whole crate has **one** `unsafe` block, `bin/roost-keeper.rs:162`,
installing SIGTERM/SIGINT handlers, with a SAFETY comment. There is no safe API for
it and no std wrapper, so it cannot be refactored away without changing keeper
shutdown behaviour. "Inherit and lose the override" is not a lint-policy trade-off;
it is a build or no build.

The drift is detectable, not merely accepted: `cargo xtask lint` flags a crate that
restates a workspace key without `workspace = true`, and `roost-keeper` is the single
entry in `xtask/src/lint_table.rs`'s `COPY_EXEMPT`.

## Defects found that no compiler would find

All are in files that compile. All are the same shape: something that was supposed
to fail loudly was made to fail quietly, or a comment described a relationship the
code does not have.

1. **`roost-keeper` was never under the workspace lint table.** Above.
2. **`session/respawn.rs` — a second `/dev/urandom` read for a trace id, with
   `.ok()`.** `session::ids::mint_trace_id` is the named seam and it already returns
   `Result<String, MintError>`, so it already refuses on entropy failure. The
   hand-rolled read threw that refusal away. Two answers where one can fail and the
   other cannot report that it did. Now calls the seam.
3. **`session/resize.rs:269` — `drop` on a `&mut SessionRecord`.** `drop` takes
   ownership, so it dropped the borrow and did nothing; the `MutexGuard` is owned by
   `resize_channel`. `close_capture` now RETURNS the history loss and the caller
   reports it after the real `drop` — in **both** arms, because the keeper-refused
   arm returned early and never reached the real drops either.
4. **`runtime/keeper_probe.rs` — `keeper_binary_digest` had no test at all.** Three
   arms distinguishable only by their fallback, so a re-shape that collapsed
   "unreadable" into a digest, or dropped the `JoinError` arm, compiled identically.
   `tests/keeper_probe_digest.rs` now pins all three by value with a golden SHA-256
   vector.
5. **`runtime/link_wire.rs` — `ProtoLinkWire` had no test at all**, and it is a real
   implementation on a path that used to refuse. A round trip through it alone proves
   nothing, because a second hand-rolled mapping round-trips against itself
   perfectly. `tests/link_wire_parity.rs` compares against `coord_worker_proto`
   directly, which is the only assertion with teeth.
6. **Three constructs whose only purpose was to keep the compiler quiet** —
   `agent_occupancy.rs`'s `let _ = self;`, `attachment_transfer.rs`'s `let _ = now;`,
   and `outbox.rs`'s `let _ = frame;` in a drain loop. All three removed; the unused
   parameter is now `_now` with a doc line saying why it is there. The question to
   ask of any file is not "does it compile" but "is anything here only there to stop
   the compiler complaining".

## Row W2 — OBSERVED, and it bit

`a_delta_past_the_row_cap_becomes_a_viewport_only_full` did not exist in any file
under `crates/roost-term`. It now does, in `crates/roost-term/tests/
emitter_row_cap.rs`, alongside a second test for the other direction — a cap that
always fires is not a cap, and without the second test a mutation that deleted the
whole arm would pass the first.

The mutation window was run with the wave-gate discipline (backup outside the
worktree, `trap` restore, sha256 checked before and after, green baseline required
before the mutation):

| step | result |
|---|---|
| baseline | 2 passed, 0 failed |
| mutation applied (`> LIVE_DELTA_SCROLLBACK_ROWS_CAP` → `> u64::MAX`) | sha256 differed, so the mutation was live |
| **must-fail** `a_delta_past_the_row_cap_becomes_a_viewport_only_full` | **FAILED**, as required |
| **must-still-pass** `a_delta_within_the_row_cap_stays_a_delta` | passed |
| rest of `roost-term` | every other target green; only the mutated target failed |
| restore | sha256 matches pre-mutation |

`scripts/row-w2-mutation.sh` holds the window and is re-runnable.

**Row W7 is SATISFIED** — `crates/roost-term/tests/dyn_dispatch_parity.rs` is the
test the row asked for. The integrator is correcting the row table; that file is
theirs.

## The `is_err()` rule, applied here

A test that asserts `is_err()` claims that *something* did. There were 26 such sites
in the worker tests; most of the `is_none()` ones are legitimate (an `is_none()` on a
precise field has no other way to be absent). Three were real and are now pinned with
`matches!` against the variant they mean:

- `tests/worker_boot_order.rs` — `OutOfOrderStep { step, at }`.
- `tests/session_vocabulary.rs` — `channel_fsm::Refusal::Terminal`, which is the
  exactly-once guarantee row W1 guards.
- `tests/worker_shutdown_boundary.rs` — `SnapshotError::Unavailable`, not
  `Unencodable`; the two say opposite things about whether there was anything to
  encode.

## Known values that are inferred, not ported

- **`host/tool_path.rs`'s `TOOL_TIMEOUT = 10s` is a v3-introduced guard.** v2 ran its
  tools through `Bun.spawn`, which has no timeout at all. **It must never be
  described as parity.** Kept deliberately — the slow caller is `gh` over the network
  and the point of the bound is to release a sampling thread — and labelled in the
  constant's own doc comment, not only in a commit body.
- **`link_serve.rs` sends `capabilities: Vec::new()`,** which encodes to zero bytes
  and so is byte-identical to a build with no such field. The comment there is right
  and the reason is a real dependency: a capability cannot be advertised until the
  `browser_commands::Deps` implementations are production, because advertising one
  the worker cannot serve is worse than admitting none.

## Open items, in order

1. **Clear the remaining test-target errors.** Seven slices were fanned on the
   no-lock rule; all had reported and their fixes are on disk, and the arbiter run
   over all three crates is the arbiter for the wave.
2. **Per-crate clippy, one crate at a time**, reporting each separately. Never
   infer one crate's clippy status from another's or from a pass count.
3. **Two agreeing `cargo test -p roost-worker -p roost-keeper -p roost-term
   --no-fail-fast` runs** with their `Running` and `test result` counts.
4. **W-2 proper**, in dependency order: L1 link wiring; L2 snapshot/reconcile; L3
   senders + durable store; L4 bootstrap redemption (**in flight** — it is what
   unblocks the CLI track's fresh-host deploy); L5 residue.
5. **The constructor**, which is mine: `runtime/mod.rs` (closing `:148` and `:180`),
   `bin/roost-worker.rs`, and the `Deps` trait in `browser_commands/mod.rs`. v2
   `main.ts` order exactly: door → session manager → agents → heartbeat → link →
   reconcile → snapshot → `Readiness::advance(Reconciled)`. `browser_commands::
   presence.rs` is a deliberately thin production impl and its header carries the v2
   citation for why — do not add invented state to it.
6. **`SessionTable` keyed `u16` beside a branded `ChannelId`** is a known residue,
   deliberately not fixed in W-C. Three files under three owners, and a type cutover
   mid-compile-fix is how a wave ends half-applied. Its own commit after the
   checkpoint. Precedent: `SessionManager::close_channel` reads the branded id off
   the record rather than converting.
7. **A test that reloads the host's service manager.** `host/install.rs`'s
   `reload_unit_manager` spawns a real `systemctl --user daemon-reload`, and two of
   the three `worker_retire_authorization` tests now exercise it. Best-effort and it
   never fails the spend, so not a correctness problem — but the principled fix is
   that **there is nothing to reload when the definition being edited is not the
   machine's real service definition**, which is exactly the case in a test that
   overrides the path.

## Uncompiled work, named

Verified by reading, never by a compiler. Small and additive, but not green:

- `session/spawn.rs` — manual `impl Debug for SpawnContext<'_>`, 15 lines. A derive
  cannot work: all four fields are `&dyn` or a `&WorkerFp`.
- `host/samples.rs` — `#[derive(Debug)]` on `HostSampler`.
- `roost-keeper` — `Debug` for `Keeper` (derive), `Server` (hand-written, because a
  derive would dump every channel's PTY and history into any log line, and
  `UnixListener` has no `Debug` anyway) and `OutputRing` (hand-written, because it
  holds a `crossbeam` `Receiver`, which has no `Debug`). `server.rs` is at 393 lines
  with 7 of headroom; it is the file to watch.

## The honest size of what is left

The remaining W-2 is ~15,000 lines of v2 TypeScript: the link senders and durable
store, reconcile, heartbeat, terminal search (890 lines, currently a test-fake-only
`Deps` trait), the local door's transport (1,470), the terminal view/peer/input
owners (1,080 of a 5,243-line tree), the attachment direct sockets (3,412), and the
whole agents subsystem (4,837). `runtime/mod.rs:148` cannot be closed by anything
smaller than a real door server plus the three owners it wires. Named here rather
than discovered at the Phase 2 gate.

## Decisions taken on the open questions L4 raised

**The Connect transport is `reqwest`, not `connectrpc`'s `client-tls`.**
`connectrpc::client::HttpClient::plaintext()` rejects an `https://` URI by design,
so a coordinator behind TLS needs either the `client-tls` feature or a
`ClientTransport` of our own. `enroll` is generic over `T: ClientTransport` so the
choice was left open, and here it is: **implement `ClientTransport` over
`reqwest`**, which the plan already pins with rustls and which `roost-cli` already
depends on. `connectrpc`'s `client-tls` would be a second TLS stack in the tree
that the plan did not pin, and this product dials a tailnet coordinator that will
eventually need its own trust configuration — which is a `reqwest` client builder
question, not a `connectrpc` feature flag.

**`Err(EnrollmentError)` aborts the boot.** L4 built it that way and I agree: a
coordinator that *answered no* leaves the machine unauthorised with the token still
unspent, and continuing would bring up a worker that cannot register. Only the
*unreachable* case continues, which is v2's actual reason for tolerating failure
and is now a distinct return value rather than a swallowed error.

**v2's blanket "may be already used" tolerance is deliberately narrowed, and the
evidence is worth keeping:** a second redeem of a spent token by the SAME key
returns the existing row and succeeds; a DIFFERENT key gets `INVALID_GRANT`. So
"already used" is precisely the case that works, and v2's handler described a
success as a failure. Tolerating every error therefore hid real refusals. The split
is by `ErrorCode`: did-not-answer continues, answered-no aborts.

**`resolveTailnetDnsName` stays cut from register.** `coordinator.proto:40-45` has
the heartbeat re-resolve the reachable address every beat precisely so a machine
rename self-heals, which makes the register-time value a one-shot best effort.
Asked the CLI lead in case their deploy path disagrees; ~30 lines to restore.

**`runStrictEnrollment` stays cut.** Its mechanism — do not spend the token until
registration is proven — is the direct opposite of the read-at-point-of-use,
spend-then-erase decision already made, and nothing in v3 calls it because there is
no `roost add-machine` installer yet. If the CLI needs it, it is a separate
function, not a mode of `enroll`.

**`delete process.env.ROOST_BOOTSTRAP_TOKEN` cannot be ported.**
`std::env::remove_var` is `unsafe` in edition 2024 and the workspace sets
`unsafe_code = "forbid"`. It is also harmless: the in-process environment is not
persisted, and the DEFINITION — the only thing a later activation reads — is erased
through the existing `host::install::scrub_service_definition_env`.

## The `Deps` impls, as `file:line` and not as a count

Per `a2861ba1`, every member of `browser_commands::Deps` and where it is actually
implemented:

| trait | production impl | test fake |
|---|---|---|
| `SessionLifecycle` | `src/session/respawn.rs` (`SessionManager`) | `tests/browser_command_support/fakes.rs` |
| `RetainedGrid` | `src/session/retained_grid.rs` (`SessionGrid`) | `tests/browser_command_support/fakes.rs` |
| `FileCommands` | `src/browser_commands/file_commands.rs:280` (`LocalFiles`) | none |
| `AttachmentStore` | `src/browser_commands/attachments.rs:197` (`SessionAttachments`) | none |
| `PresenceReports` | `src/browser_commands/presence.rs:85` (`WorkerPresence`) | `tests/browser_command_support/fakes.rs` |
| `ScrollbackSearch` | **NONE** | `tests/browser_command_support/fakes.rs` |

**One zero, and the reason it is not a search artefact — which is a category
difference, not a stronger search.** Main's privatisation test ("make it private
and the crate still builds") is the right upgrade for a symbol nothing references,
and it does not apply to this one, because of what a trait impl IS. An inherent
impl on a type in its own file is reached by being compiled and has no
reference anywhere — that is the `sync_ws::egress` shape, and it is invisible to
every search. **A trait impl cannot be reached that way: the trait name must be
written at the impl site or the type does not implement the trait.** So
`impl ScrollbackSearch for` matching nothing across `src/` is not a weak negative
that needs a compiler to confirm; the impl text either exists or the type does
not implement the trait. There is no spelling of it that a grep would miss.

**What the zero means substantively:** terminal search is unported. It is 890
lines of v2 TypeScript and it is the one `Deps` member with no production owner,
so the `Deps` criterion is not met until it lands. `browser_commands/presence.rs`
is the contrasting case and is deliberately thin — its own header carries the v2
citation for why v2's worker holds no cursor and no title, and a reviewer should
read that header before "improving" it.

## BEFORE YOUR FIRST BUILD: `target-track` had a destructive guard run against it

I ran the now-withdrawn form of the disk guard on this directory:
`rm -rf debug/incremental` and
`find debug/build -maxdepth 2 -name out -type d -exec rm -rf {} +`. That deletes
each build script's `out/` **without** its matching
`debug/.fingerprint/<pkg>-<hash>-*`, which leaves a record of a success with no
result and produces `couldn't read .../out/private.rs` on the next build.
`WebLeadU2` hit exactly this on their directory.

**Evidence about this directory's state, and it is evidence rather than
reassurance:** the full three-crate arbiter that ran *after* the guard
(`cargo check -p roost-worker -p roost-keeper -p roost-term --all-targets
--keep-going` at `be399403`) completed and returned a complete diagnostic set
including every test target, so the directory is sound for everything already
built in it. **What is unverified is any crate that had not yet been built here**,
because nothing in that run needed one, and that is precisely the population the
guard damages.

**So: a first build that fails with `couldn't read .../out/private.rs` is THIS,
not your tree.** The source is untouched. The remedy is to clear the affected
build-script FINGERPRINTS as well as their outputs, or `cargo clean` the
directory — never to re-run the same deletion, which is what produced this.

**First command: `cargo check -p roost-worker --lib`, not `--all-targets`.** It
covers the library and the three unmeasured `bootstrap_redeem/` files — including
the `pub` -> `pub(super)` narrowings nobody has compiled — without dragging in the
sixteen test-target errors that are not yours. It is `check` and not `clippy`, so
it will not catch the `unused_imports` / `collapsible_if` class; that is the next
command, not this one.

**And the lesson, because it is the reason this is at the top rather than a
footnote.** I applied the guard to a directory that was mid-*wave* rather than
mid-*build*, it happened to be harmless, and my arbiter then rebuilt cleanly
afterwards. I had the evidence that would have shown the guard was dangerous — a
build that succeeded after the deletion — and read it as confirmation. The defect
is only visible in a directory mid-rebuild, which is a state a lead visits briefly
and by accident. **A guard that is safe only when there is nothing to lose is not
a guard**, and I had evidence of that and drew the opposite conclusion.

---

## 2026-09-27 — LEAD CHANGED HANDS to WorkerLeadW3 (third lead)

`WorkerLeadW2` stood down; this section is what the third lead did. **The
sections above it are the reasoning that produced the tree and are still
accurate; the sections below are a moment, not a state.** Read the worktree
before trusting any number here.

### The `v3` merge is in, and M1 needed no resolution — see the property to verify at the end of this section

`ort` merged clean across 15 files, **none of them mine**. The specific hazard
was checked rather than assumed: `crates/roost-keeper/Cargo.toml` does **not
appear in the merge diff at all**, because `v3` never touched that file. So the
copy-form table came through untouched — M1 resolved by *not being in
conflict*, not by a lucky auto-merge. Post-merge form verified: `[lints.rust]`
and `[lints.clippy]`, and **no `workspace = true` under `[lints]`**.

The merge diff over my three crates was **empty**, so no W-C work is at risk
from it.

The merge brought `xtask/src/lint_table.rs` into existence, with
`roost-keeper` as the single `COPY_EXEMPT` entry. The manifest comment already
named `COPY_EXEMPT`, so the two are consistent now. **`cargo xtask lint` is
only runnable after this merge** — before it, the rule and the allowlist entry
do not both exist.

### The build directory was damaged, and the repair is a judgement call worth knowing

The first `--all-targets` run died with `couldn't read .../out/private.rs` on
`serde_core`, `thiserror`, `rustversion` and `libsqlite3-sys` — the withdrawn
disk guard's signature, predicted by the section above. 77 of 115 `build/*/`
directories were missing `out/`.

**What was done, and the honest caveat:** the 31 broken package names were
globbed as `.fingerprint/<pkg>-*` and **269 real directories across 31
packages were removed** (fingerprints went 1383 -> 1114). This is *not* the
withdrawn recipe — that one paired `build/*/out` against a **same-named**
`.fingerprint/` *directory* and matched 0 of 7, and a zero match is the tell
that a probe is guessing. **But the correct pairing is not established** and I
did not establish it; the removal rests on a naming correlation.

**It worked** — the next `--keep-going` run completed and produced a full
diagnostic set, which is the experiment that settles it. After the integrator's
broadcast, `cargo clean` on one's own directory is the only sanctioned reclaim
and nothing more surgical should be published.

**Sizes, correcting a figure that was quoted as 15 GiB:** `debug/deps` is
**5.4 GiB**, `debug/build` 375M, `.fingerprint` 22M, whole `target-track`
**5.7 GiB**. It is not 15 GiB and it is not `roost-keeper`'s alone — it is the
shared dependency graph for all three crates. The conclusion is unchanged and
still not worth a build.

### The test-target errors: what was actually wrong, and one error of mine

`--keep-going` on `roost-worker` after the merge returned **10 errors in 4
targets**, all second-order. Two things about that number:

- **Per-target attribution is unreliable, and the handoff's own warning about
  it is what caught the real shape.** `session_support/mod.rs` is compiled into
  **four** targets (`session_adoption`, `session_binding`, `session_lifecycle`,
  `session_resize`), so one defect in it is reported as four single-error
  targets. The inherited table listed `session_lifecycle` 1, `session_adoption`
  1, `session_resize` 1 and `session_binding` 1 — **that was one defect, not
  four**, and the same mechanism hid the `env_key` and `E0451` errors behind
  the `session_spawn` ones.

**What each was, and the fix:**

| target | error | cause | fix |
|---|---|---|---|
| all 4 session targets | `TerminalState: Default` unsatisfied | `ScriptedKeeper` derived `Default`; `TerminalState` has no `Default` | hand-written `Default` with an explicit `80x24`, NOT a `Default` on `TerminalState` |
| `session_spawn` | E0451 x5 | `FixedResolver.cwd` private, built by struct literal | `FixedResolver::at(&str)` constructor |
| `session_spawn` | E0616 x4 | `FakeKeeper.opened` private, read directly | `opened_channels()` accessor — **narrowing, not widening the field** |
| `session_spawn` | E0515 | `context()` held `&worker_fp()`, a borrow of a temporary | fingerprint becomes a **parameter**; the caller owns it |
| `host_identity_facts` | E0515 | closure returned `as_deref()` of a temporary | see below — **I got this wrong first** |
| `worker_boot_order` | E0308 | `MapEnv::with` takes `&str`, given `String` | bind the `String`, pass `&home` |
| `worker_retire_authorization` | E0599 `env_key` | `env_key` is an associated fn with no `self` | `Definition::env_key(host)` |
| `keeper_pool_spawn` | E0382 moved `marker` | `(spawned, marker, record.printed(&marker))` — tuple elements evaluate in order | read the text into a local first |

**`os_release_value` returns `Option<Cow<'a, str>>`, not `Option<String>`.** My
first fix assumed `String`, which turned 1 error into 3. The correct shape
derefs at the **comparison**, not inside the closure: the `Cow`'s lifetime is
tied to `source`, so returning it whole is fine and `value("K").as_deref()`
keeps the temporary alive for the whole statement.

### Unused imports: the compiler's list had a stale entry

Removing the allow and reading the lint list, per the opposite trade:

- `session_support/mod.rs:9` `roost_keeper::history::HistoryRecord` — removed.
  Note the brief's claim that this was unused in **`session_adoption`** was
  wrong: it is used **8 times** in `session_adoption.rs` and was unused in
  **`session_support/mod.rs`**. Same name, different file.
- `session_adoption.rs:13` and `session_resize.rs:9` `roost_term::TerminalCore`
  — removed. Both genuinely unreferenced.
- `credential_support/scratch.rs:36` `Scratch::root` — **used by three other
  binaries** (`worker_retire_authorization`, `shell_spec_resolution`,
  `host_folder_facts`). `#[allow(dead_code)]` with that reason; narrowing or
  deleting it would break a caller to silence a lint in `keeper_probe_digest`.
- `session_support/mod.rs` now carries `#![allow(dead_code)]`, and the reason
  names the four binaries and **which one calls what**: `closed_events` is
  `session_lifecycle`'s alone; `with_survivor`, `delivered`, `killed` are
  `session_adoption`'s alone. Those four names are the entire lint list, read
  from a run with the allow deleted.

### `enroll` still has no caller — confirmed by reading, and it is W-2's work

`bootstrap_redeem::enroll` (`runtime/bootstrap_redeem/mod.rs:131`) has **no
caller anywhere** in `src/`, `tests/` or `bin/`. `bootstrap_redeem` is declared
`pub mod` at `runtime/mod.rs:26` and nothing calls into it. The composition
root is what closes this.

**Also: the brief says L4 made three `pub` -> `pub(super)` narrowings. There
are two** — `label.rs:79` and `label.rs:94`. The third may be a `pub fn` that
should have been one, or the count is off; either way the confirming build has
to cover it and nobody should take "three" as measured.

### Where the compilation actually stands

**Read the worktree first. This whole section is a moment, not a state.**
`git log --oneline -8` and `git status --porcelain` are the first two commands.
Where a commit is named below it names a **property to verify**, never a tip to
start from.

### The `v3` merge is in, and M1 is intact — verify the property

`git merge-base --is-ancestor v3 HEAD` exits 0. The merge resolved with no
conflicts, and **it touched no file in `roost-worker`, `roost-keeper` or
`roost-term`** — check with
`git diff --stat <pre-merge-tip> HEAD -- crates/roost-worker crates/roost-keeper crates/roost-term`,
which must print nothing.

The hazard worth re-checking every time: **`crates/roost-keeper/Cargo.toml` was
not in the merge diff at all**, because `v3` never touched it. So the
copy-form table came through untouched — M1 resolved by *not being in
conflict*, not by a lucky auto-merge. Verify directly: the file has
`[lints.rust]` and `[lints.clippy]` and **no `workspace = true` anywhere under
`[lints]`**. That absence IS the repair; if a future merge brings a `[lints]`
block back, the crate is un-gated and the 19 production `expect()`s are legal
again.

The merge is also what made `cargo xtask lint` runnable, by bringing
`xtask/src/lint_table.rs` into existence with `roost-keeper` as the single
`COPY_EXEMPT` entry.

### A gate failure that was ALREADY there: `session_support/mod.rs` was over the cap

`xtask/file-size-baseline.json` is **`{}`** — empty. So a file absent from it
may never exceed 400 lines, and `session_support/mod.rs` was **445 at the tip
before this lead touched it**. It was a live `cargo xtask lint` failure that no
one had run, not something the error-clearing introduced; this lead's edits took
it to 485, which is worse and is why it got fixed now rather than later.

**Split into two files under the same module directory**, so the four binaries'
`mod session_support;` is unchanged:

- `session_support/fakes.rs` — the fakes (`PinnedClock`, `RecordingSink`,
  `ScriptedKeeper`, `RecordingDelivery`, `CountingCells`, `NeverSpawns`,
  `FixedResolver`, `SharedDelivery`, `shared_delivery`, `event_kind`).
- `session_support/mod.rs` — the id/shell helpers, the constants, and `Harness`.

Both are under the cap. **Only `PinnedClock` and `ScriptedKeeper` are
`pub use`d**, because those are the only two the four binaries name; the rest
are a private `use`, since `Harness` is their only consumer and a `pub use`
nothing reaches is its own lint inside a private module.

**Three import mistakes the split caused, all of the same kind** — a name that
was used in the old single file is used in the new one, and moving code moves
the import with it:
`KeeperChannels` (dropped, but `Harness::with_keeper` casts to
`Arc<dyn KeeperChannels>`), `Reservation` and `ChannelBinding` (kept, now
unused), and `shell_spec` in `fakes.rs` (kept, unused — `FixedResolver` holds
a `ShellSpec` the *harness* builds). This is the same attribution trap as the
error counts: a shared module's breakage is reported against whichever
consumer noticed.

### Pre-measured, so the clippy pass starts from facts rather than from the brief

These were measured on this tree, not carried across a context boundary, and
they are the inputs a per-crate clippy pass should be read against:

- **`expect`/`unwrap` outside `#[cfg(test)]`: ZERO in all three crates' `src/`.**
  Re-measure with the same per-file `awk` split, because a plain `grep` counts
  the test modules too and the test-side `expect`s are legitimate.
- **`#[allow(...)]` in `src/`: exactly ONE in the whole track** —
  `#[allow(clippy::too_many_arguments)]` on `SessionManager::new`
  (`session/lifecycle.rs:202`), which is the dependency-injection constructor.
  `roost-keeper` and `roost-term` have **no allow attribute at all**. So the
  "19 `expect()`s removed with no allow added" claim holds, and stronger than
  stated: no allow was added anywhere in `roost-keeper`.
- **`roost-keeper`'s `unsafe_code` audit: ONE `unsafe` block in the entire
  crate**, at `src/bin/roost-keeper.rs:162`, with a SAFETY comment at `:160`
  naming async-signal-safety. It is in the **binary** target; the library has
  none. So the `[lints.rust] unsafe_code = "allow"` override is needed for
  exactly one site and the crate cannot be built without it.
- **Test binaries: 77, not 97.** 52 in `roost-worker`, 20 in `roost-keeper`,
  5 in `roost-term`. The 97 figure counts `.rs` files *including* the
  `*_support/` modules, which are compiled INTO a binary rather than being
  one. This is the same conflation as the 15 GiB figure, and it is the honest
  input to any "fewer test binaries" discussion.

### W-2 sizing, measured on both trees rather than carried across

The brief's "~15,000 lines of v2 TypeScript remaining" is a subset sum I have
not independently derived. What I *did* measure, so the next lead can derive it
or correct it:

**v2 `apps/worker/src` totals 38,329 lines.** The per-domain figures quoted in
the briefs are exact matches against that tree — `attachments/` 3,412,
`agents/` 4,837, `terminal/` 5,243 — so the brief is not inventing them. The
others, measured: `session/` 6,061, `transport/` 5,321, `keeper/` 4,333,
`diag/` 2,626, `local-door/` 1,853, `host/` 1,643, `boot/` 1,077,
`browser-commands/` 646, `util/` 188, plus 1,089 at the top level.

**v3 `crates/roost-worker/src` is 19,515 lines**, and the per-domain shape says
exactly which domains are ported and which are stubs:

| domain | v2 | v3 | reading |
|---|---:|---:|---|
| `session/` | 6,061 | 5,831 | essentially ported |
| `host/` | 1,643 | 3,016 | ported, and more verbose in Rust |
| `browser_commands/` | 646 | 2,317 | ported; one member (`ScrollbackSearch`) is test-fake-only |
| `keeper_pool/` | (in `keeper/`) | 906 | ported |
| `runtime/` | (in `boot/`, `transport/`) | 3,329 | the W-2 wiring target |
| **`agents/`** | **4,837** | **78** | **a stub** |
| **`attachments/`** | **3,412** | **53** | **a stub** |
| `peer/`, `door/` | (terminal, local-door) | 53, 48 | stubs |
| **`terminal/`** | **5,243** | **no such dir** | terminal core lives in `roost-term`; the view/peer/input owners do not exist yet |
| `diag/` | 2,626 | — | not started |

**The two stubs with the largest denominators are `agents/` (78 of 4,837) and
`attachments/` (53 of 3,412).** Those two plus the missing `terminal/` owners
are the bulk of the distance, and they are the reason `UNIMPLEMENTED: = 3` and
`ScrollbackSearch`-test-fake-only are the right two things to report as the
track's distance from done: both are symptoms of the same fact, which is that
the wiring has three named owners that do not exist yet.

## Where the gate stands when this lead's context ended

Read the worktree first. Every line below is a property to verify, not a
number to trust.

**GREEN, and measured:**

- `cargo check -p roost-worker -p roost-keeper -p roost-term --all-targets
  --keep-going` → **0 errors, 0 warnings.** A total: `--keep-going` on, and no
  parse error masking a class behind it.
- `cargo fmt --check` → clean (exit 0), all three crates.
- `cargo clippy -p roost-keeper --all-targets -- -D warnings` → **exit 0**,
  lib + bin + 20 test targets reached. The crate's first gate result ever.
- `cargo clippy -p roost-term --all-targets -- -D warnings` → **exit 0.**

**`exit 0` is a TOTAL, not a floor.** The floor rule is about how many
diagnostics a run saw; a run that saw all of them and found none is bounded by
nothing, because there was no first failing target. `exit != 0` gives a count
bounded by where it stopped.

**RED at the moment of the context end, and this is the next command:**

- `cargo clippy -p roost-worker --all-targets -- -D warnings` → **exit 101.**
  The six lib lints are fixed (two `collapsible_if`, `double_parens`,
  `needless_borrow`, explicit `.into_iter()`, `manual_inspect`) and the
  re-run confirmed the lib is clean by progressing into the test targets,
  where it found one more: `assert_eq!(x.is_ok(), true)`. **That is fixed and
  committed but NOT re-verified.** So the honest state is: the library is
  clean by measurement; the test targets have been reached once and had one
  diagnostic, now fixed; **the full `--all-targets` run has never come back
  clean and must be re-run before this crate is called green.**

**NOT RUN AT ALL — do not read anything above as covering these:**

- **No `cargo test` has been executed on this track.** There is no
  `Running`/`test result` count, so the two-agreeing-runs criterion is
  **unmeasured, not failed**. Any pass count quoted for `roost-worker`,
  `roost-keeper` or `roost-term` is inherited from a predecessor's transcript
  and is not a measurement of this tree.
- `cargo xtask lint` has never been run here. It is runnable now that `v3` is
  merged. **The one thing known in advance: `session_support/mod.rs` was 445
  lines against an empty baseline and is now split, so that specific
  violation is fixed — but the other rules are unmeasured.**

**Two questions the integrator asked that this track could NOT answer, and why
not — do not let either be treated as answered:**

1. Whether `clippy.toml`'s `allow-unwrap-in-tests` keys on the enclosing
   function or on the compilation unit. `clippy.toml` states the setting
   (`allow-unwrap-in-tests = true`, `allow-expect-in-tests = true`) and its
   reason, but **the file does not state the semantics**, and reading a
   setting is not reading what it keys on.
2. Whether a **module-rooted** `#![allow]` reaches the modules that include
   it. **The instance on this track does not answer it**, and the reason is
   worth stating so nobody mistakes it for evidence:
   `credential_support/scratch.rs` declares `#![allow(dead_code)]` at its module
   root and its nine consumers declare nothing, and the warnings it used to
   raise did disappear from all nine. **But `root` and `path` are defined IN
   `scratch.rs`, so that allow only ever had to cover its own module's items —
   which is trivially true and says nothing about reaching a consumer's code.**
   Answering it needs a fixture whose *consumers'* sites are what the allow
   covers, and a clippy run that has reached them.

---

## 2026-09-27 — LEAD CHANGED HANDS to WorkerLeadW4 (fourth lead)

**Read the worktree before trusting any number above.** Every figure in the two
preceding sections is a property to verify, not a tip to start from. What
follows is what this lead measured, and where it contradicts the sections above
it says so.

### M1 HAS CHANGED STATUS. It is now a live defect on `v3`, not a resolved non-conflict

W3 recorded M1 as resolved "by *not being in conflict*", because `v3` never
touched `crates/roost-keeper/Cargo.toml`. **That reading was correct when written
and is no longer true of `v3`.** Measured on this lead's tree:

```
$ git show v3:crates/roost-keeper/Cargo.toml | sed -n '/^\[lints/,$p'
[lints.rust]
unsafe_code = "allow"          # plus a comment
```

**`unsafe_code` and nothing else. No `[lints.clippy]` block at all, and no
`unwrap_used` / `expect_used` / `todo` / `unimplemented` deny, no
`rust_2018_idioms`, no `missing_debug_implementations`.**

**And this is the MERGE BASE form too** — `9338d1ef` is byte-identical to `v3`'s.
So it is not a fresh regression on the integrator's side; it is the form the file
has always had there, and `xtask/src/lint_table.rs`'s `COPY_EXEMPT` entry is what
has been permitting it.

**Why it matters, and it is the exact sentence the integrator wrote as M1's
reason:** a crate-level `[lints.rust]` that restates `unsafe_code` *without*
`workspace = true` does not merge with the workspace table — it **replaces** it.
So on `v3` today the crate has no deny table, **and the 19 production `expect()`s
W2 removed are legal again.** The mechanism M1 exists to catch is confirmed; only
the side it applies to has flipped.

**This lead's tip carries the correct copy form** (`[lints.rust]` with
`unsafe_code` + `missing_debug_implementations` + `rust_2018_idioms`, and a
`[lints.clippy]` with the four denies, and **no `workspace = true`**). The merge
**will conflict on this hunk and must take this side.** A successor merging by
pattern-match will take the shorter block and silently un-gate the crate.

### The two `clippy.toml` questions — still unanswered, and now with a REASON they persist

Both are still open, and this lead did not guess either. Recorded here because
**the reason they persist is now a fact, not a shrug**:

1. **`allow-unwrap-in-tests` — function or compilation unit?** Open. `clippy.toml`
   states the setting and its reason and **not** the semantics. A successor must
   not read the answer out of the file.
2. **Does a module-rooted `#![allow]` reach the modules that include it?** Open,
   and the `credential_support/scratch.rs` instance is **still not evidence** —
   for the reason W3 gave, which this lead re-read and agrees with verbatim:
   `root` and `path` are defined *in* `scratch.rs`, so the allow only ever had to
   cover its own module's items, which is trivially true.

**What a real answer needs**, so the next lead does not spend the same hour: a
fixture whose **consumers' own sites** are what the allow covers, plus a clippy
run that has **reached** those consumers. Answer 1 needs a `#[cfg(test)] mod`
inside a **library** crate carrying the declaration, since that is the position
the CLI track's three `src/` files are waiting on. Neither exists yet.

### Standing unverified state — one entry REMOVED, and its count was wrong twice

`crates/roost-worker/src/runtime/bootstrap_redeem/label.rs` (`pub(super)`
narrowings, recorded as never having been seen by a compiler) is **removed**:
`cargo check -p roost-worker --lib` has run over the module, and the crate's
`--all-targets` check reached 0 errors. The module compiles and the narrowings
are real code, not a guess. `bootstrap_redeem` is declared `pub mod` at
`runtime/mod.rs:26`, so it is in the lib's module tree and the lib check reaches it.

**The count had been wrong twice and is now four.** The handoff said three
(`:32,40,79`); W3 read two (`:79`, `:94`); the file actually has **four**
`pub(super)` declarations:

| line | item |
|---|---|
| 32 | `pub(super) trait LabelSources` |
| 40 | `pub(super) struct HostLabelSources` |
| 79 | `pub(super) fn resolve_worker_label` |
| 94 | `pub(super) fn named` |

(`:20` is `pub const ENV_WORKER_LABEL`, not a narrowing.) **Both prior counts
omitted sites, in opposite directions — W3's "two" dropped `:32` and `:40`,
which are the trait and the struct, i.e. the two most load-bearing items in the
file.** A count taken from a brief or from a predecessor's note is not a
measurement; `grep -n pub` is.

### Track W's distance from done — CORRECTED, and it is WORSE than the table above says

**The `Deps` table in the W2 section above has six rows and is missing one.** It
was wrong in the flattering direction, so it is corrected here rather than
amended in place.

`src/browser_commands/mod.rs:259-271` — `Deps` has **SEVEN** trait-object
fields, not six. Measured impls across the whole crate:

| field | trait | production impl | test fake |
|---|---|---|---|
| `sessions` | `SessionLifecycle` | `src/session/respawn.rs:288` | yes |
| `presence` | `PresenceReports` | `src/browser_commands/presence.rs:85` | yes |
| `files` | `FileCommands` | `src/browser_commands/file_commands.rs:280` | — |
| `grid` | `RetainedGrid` | `src/session/retained_grid.rs:62` | yes |
| **`search`** | **`ScrollbackSearch`** | **NONE** | **yes** |
| `searches` | concrete `Mutex<Searches>`, not a trait | n/a | n/a |
| `attachments` | `AttachmentStore` | `src/browser_commands/attachments.rs:197` | — |
| **`diagnostics`** | **`DiagnosticReports`** | **NONE** | **yes** |

**`DiagnosticReports` (`src/browser_commands/diagnostics.rs:31`) is a SEVENTH
field and it has no production impl either:**

```
$ grep -rn 'impl DiagnosticReports' src/ tests/
tests/browser_command_support/fakes.rs:147:  impl DiagnosticReports for FakeDiagnostics {
```

**One match and it is the fake. That makes it a TOTAL, not a floor** — a trait
impl cannot hide from a name search, because the trait name must be written at
the impl site (the same argument the W2 section makes for `ScrollbackSearch`,
and it holds).

**So the honest figure is 5 of 7, and TWO `Deps` members are test-fake-only:
`ScrollbackSearch` and `DiagnosticReports`.** Every brief and the table above
have been reporting one. Track W is judged on *"no test fake is the sole impl of
a `Deps` trait"* and **the distance from that is two traits.**

**What this adds to W-2 sizing:** `DiagnosticReports` is a gap no sizing pass
has counted. v2 `apps/worker/src/diag/` is **2,626 lines** (W3's measurement)
and v3 has no `diag/` directory at all. **Before costing it, read the trait's
method set** — a handful of report-shaped getters over data the session layer
already holds is small; the v2 diag subsystem is a domain W-2 never budgeted.
Guessing here is how a 15,000-line estimate becomes wrong in the direction that
hurts.

`UNIMPLEMENTED:` independently re-measured at **3** — `runtime/mod.rs:149`,
`runtime/mod.rs:181`, `runtime/snapshot_source.rs:39` — and **zero** `todo!()` in
`src/`. `runtime/mod.rs:149` needs a real local-door server plus the three owners
it wires; that is not closable by anything smaller.

### Row W2 — the mutation site is `:127`, and `:29` is a decoy that reads plausible

Measured, because the brief handed `:29` and the row table says `:127`:

```
src/emitter.rs:29    pub const LIVE_DELTA_SCROLLBACK_ROWS_CAP: u64 = 250;   <- DEFINITION
src/emitter.rs:127   ... > LIVE_DELTA_SCROLLBACK_ROWS_CAP;                  <- the ARM
src/lib.rs:54        pub use emitter::{..., LIVE_DELTA_SCROLLBACK_ROWS_CAP, ...};  <- RE-EXPORT
```

**`:29` cannot bite, and the reason is a property of the TEST, not a typo.**
`tests/emitter_row_cap.rs:60` derives its own input from the constant it
guards — `let overflow = LIVE_DELTA_SCROLLBACK_ROWS_CAP as usize + 64;` — and
`:68` asserts `growth > LIVE_DELTA_SCROLLBACK_ROWS_CAP`. Setting the constant to
`u64::MAX` moves the overflow, the scroll amount and the precondition together,
so the escalation still fires and **the test still passes.** The test is
self-scaling by design (it survives a legitimate cap retune), and that is
exactly what disqualifies `:29` as a lever: **a site whose value the test reads
is not an independent mutation site.** Only `:127` disables the comparison;
only that makes `frame.full` false. `:54` is a re-export and changes nothing.

**The row table's `:127` is correct and the brief's `:29` is wrong.** Recorded
here because a successor reading the brief will reach for `:29`, watch the row
"not bite", and record that as a property of the code rather than of the
mutation site.

**W2 is NOT re-run by this lead.** The gate is the assignment, and a mutation
window requires a green baseline for the named binary — which does not exist on
this track because **no test has ever been executed here.** Gate first, rows
after. W1, W3, W4, W8 remain unrun for the same reason, and W1 is the most
load-bearing row on the branch.
