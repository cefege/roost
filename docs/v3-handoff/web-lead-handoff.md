# Track U — web lead handoff

Worktree `/home/almalinux/repos/roost-v3-web`, branch `v3-web`.

> **THIS NOTE IS A MOMENT, NOT A STATE. Read the worktree.** Run
> `git log --oneline -5 && git status --porcelain` in
> `/home/almalinux/repos/roost-v3-web` before acting on anything here. Those
> two commands are the state; this file is not. Where a sha appears below it
> names a PROPERTY to verify, not a starting point to resume from. A merge of
> `v3` into `v3-web` has ALREADY LANDED — do not go looking for a merge to do.

## The defect found this pass — `||` short-circuits a mutation

`clear_account_state_for_logout` and `clear_auth_scoped_state`
(`crates/roost-client-core/src/store/root.rs`) both computed

```rust
let had_any = !store.mcp_relays.is_empty()
    || !store.pair_requests.is_empty()
    || toasts::clear_all(&mut store.toasts)   // <-- MUTATES
    || transfers::clear_all(&mut store.transfers) // <-- MUTATES
    || !store.spawns.is_empty()
    || !store.pending_closes.is_empty();
```

`||` short-circuits, so the first truthy term skips every later `clear_all`.
With any `pair_request` present, **`toasts::clear_all` and
`transfers::clear_all` were never called** — the two slices that render a
machine name and a file path from the old account. `store_revision.rs` caught
the toast half; **transfers was the second victim of the same line** and the
test would have failed on it next. `clear_auth_scoped_state` has the identical
shape and no in-tree caller.

**The fix is the shape, not the operands:** run the clears first, read the
predicate off their return values. The disjunction is logically unchanged, so
the `revision` behaviour is provably identical — what changes is *which slices
get emptied*. Keep this reading when you see a mutating call inside a boolean
operator anywhere in this repo.

`CredentialsDiscarded` (`handle_event.rs:126-135`) was checked for the same
class: it is straight-line with no predicate, and is clean.

## Measured in the most recent pass — RE-MEASURE, ALL OF IT IS STALE NOW

- `cargo check -p roost-client-core --all-targets` — **EXIT=0, 0 errors, 0
  warnings** (total, from the previous lead).
- **Clippy ladder — a run reaching further is a run whose predecessor's
  findings are gone:**

  |run|stopped at|diagnostics|fix|
  |---|---|---|---|
  |A|`test "connect_interceptor"`|2 (FLOOR)|`cloned_ref_to_slice_refs` → `4c79b8de`|
  |B|`test "prefs_persistence"`|2 (FLOOR)|`bool_assert_comparison` → `9c08c10e`|
  |C|**reached the end**|**0 — TOTAL, exit 0**|—|

  **A and B are floors and must NOT be summed.** C is a total, bounded by
  nothing because there was no first failing target.
- **THE CLIPPY CONTRADICTION IS UNSETTLED IN THE RECORD.** `WebLeadU2` reported
  run C as **exit 0, a total**. `WebLeadU3` reported **never run to
  completion**, four attempts, three killed. Both are honest from their own
  vantage and they cannot both describe the same run. **A fresh single run of
  the unmodified command settles it by its exit code** — do not pick a side
  and do not average them.
- **`roost-web-terminal`: never compiled.** No clippy, no build, no number.
  Say "never compiled"; publish no number. 5,751 lines across 25 files, all
  under the 400 cap — **a line count is not a compile.**
- The last full suite figure is **211 passed / 12 failed / 0 ignored**, 34
  `test result` lines, a **total** (`--no-fail-fast`, all targets ran). **Stale
  the moment anything changes** — including the `root.rs` fix above.

## The 14-allow measurement — settled, with its reach stated

The allow is **conditional on where the unwrap lives**: `clippy.toml`'s
`allow-unwrap-in-tests`/`allow-expect-in-tests` exempt a `#[test]` BODY; they do
NOT exempt a plain helper `fn` in the same test crate, because a test crate is
its own crate and its helpers are ordinary functions.

- 9 of the 14 carrying it have helper sites — load-bearing, keep.
- 5 have ZERO helper sites — dead weight, removed: `auth_ceremony`,
  `auth_device_key`, `auth_first_boot_race`, `terminal_epoch_fence`,
  `terminal_full_before_delta`.
- 3 had helper sites and NO allow and were failing: `navigation_index` (4),
  `palette_catalog` (4), `store_selectors` (4).
- Net 14 → 12, and the list moved in **both** directions. "All-or-none" was the
  wrong instruction; "all-or-none by property" is the right one.

**THE CLASSIFYING UNIT IS THE COMPILATION UNIT, NOT THE FILE.** A crate-level
`#![allow]` is a property of the crate. For a shared fixture there are **two**
correct shapes: a consumer root declares it, **or** the fixture declares it for
itself. `tests/support/auth.rs:10` is the self-declaring shape, which is why
its 4 consumers that declare nothing at their root are correct as they stand.
`tests/support/mod.rs` has zero sites. `tests/layout_support/mod.rs` has no
helper sites and all 7 consumers declare the allow. **The only failing
combination is a fixture that does neither**, and it is invisible from the
fixture's own file — you find it by listing consumers.

**The instrument was validated, not fitted:** it predicted 4 helper sites in
`navigation_index.rs`; clippy independently reported exactly 4, while the two
`expect`s inside `#[test]` bodies produced none.

**UNCONFIRMED:** the five removals were never confirmed by a compiler — the
confirming run was killed when the tree moved under it.

## `web-sys` — a resolver figure, NOT §1's acceptance

`cargo tree -p roost-client-core -e normal | grep -c web-sys` → **0**, which is
what `docs/phase4-client-contract.md` §1 asks for. **The bare number is a
trap.** The same grep over `src/` is 0; over `tests/` it is **4, not 5** — all
four inside `tests/core_without_a_browser.rs`, at `:65` and `:100`, which is the
test that *enforces* the 0. `source_files()` scans only `src/` (`:40`) and
`declared_dependencies()` reads only `[dependencies]` (`:133`), so `tests/`
hits and dev-deps are both outside its scope by design. **Quote the resolver
figure with its scope, and report the wasm32 build as a separate compiler
fact — the grep is not the build.**

## A linter suggestion that was wrong here

`manual_clamp` on `store/layout/tree.rs` `normalize_pane_ratio` wanted
`ratio.max(MIN).min(MAX)` → `ratio.clamp(MIN, MAX)`. Taking it would have
deleted the property the function exists for: `f64::clamp` returns NaN for a
NaN input. The three guards above exist to prevent exactly that. The guards
stay. **The discriminator: whether the file already carries a comment stating
the rule.** One clippy suggestion in seven on this track was wrong about what
was load-bearing.

- `add_transfer` takes a `NewTransfer` params struct (was 8 positional
  arguments tripping `too_many_arguments` 8/7). All 6 call sites converted.
  `NewTransfer` lives in `store/transfers/record.rs` beside `Transfer`.
- `handle_sync.rs` was 405 against the cap: `apply_frame` moved to
  `src/handle_sync/apply_frame.rs`, `pub(super)`, moved body byte-identical
  apart from the one visibility change. 405 → 270, new file 149. **No file in
  this worktree is over 400.**

## Still open

- **Two agreeing GREEN `--no-fail-fast` runs** of `roost-client-core`. The
  211/12 above is red and stale. Re-measure, triage against the run rather
  than against any inherited list, re-measure again.
- The remaining failures by target, as measured (NOT re-checked since the
  `root.rs` fix — re-measure): `sync_reconnect_placement` 3,
  `browse_machine_scope` 2, `store_revision` 1 more,
  `auth_first_boot_race` 1, `layout_document_apply` 1, `layout_pane_tree` 1,
  `navigation_index` 1, `prefs_persistence` 1.
- `cargo clippy -p roost-web-terminal --all-targets -- -D warnings` —
  **the largest untested body of work on this track.**
- The wasm32 build — **run it; the grep is not the build.**
- `cargo fmt`, reading the diff. Three `edit` calls by an earlier lead clobbered
  adjacent lines and fmt does not catch that class.
- `cargo xtask lint`. Expect the fixture-allow rule to report **0** on this
  track — the failing case (a fixture with a consumer lacking the declaration)
  **does not exist here**. The red you will see in a workspace-wide run is
  `roost-keeper`'s lint copy on `v3`: the worker track's to fix, not this one.
- `git merge v3`, re-verify, push.
- `[dev-dependencies]` for roost-coord / roost-worker / roost-keeper, owed for
  the integrator's Phase 4 `headless_client.rs`. The DAG permits the edges and
  `declared_dependencies()` reads only `[dependencies]`, so §1 is unaffected.
  **A manifest edit re-resolves the lockfile under every running cargo in the
  wave — do it holding the only lock on your target directory.**

## Build environment

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-web/target-track
```

The batch script that runs the five gates in order, each figure labelled with
its bound, is at **`/tmp/u4-gate.sh`** — run it with `bash /tmp/u4-gate.sh`
and read the `*_EXIT=` lines.

**Do not run the old disk guard.** Deleting `*/debug/build/*/out` without its
fingerprint destroys the build; it cost three proc-macro crates. The only
sanctioned reclaim is `cargo clean` on your own `target-track`.

**Commit and push at every stage boundary, before starting a long build.** This
worktree has survived three near-losses. An incomplete-but-committed wave is
resumable; an incomplete uncommitted one is re-derivable only by whoever still
holds the context. Read the diff before committing it.
