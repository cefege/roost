# Wave-B mutation survey — shared brief (lead: WebLeadU3)

## Rules (verbatim from the lead's assignment; binding)
- Absolute paths under `/home/almalinux/repos/roost-v3-web` only; never touch `/home/almalinux/repos/roost` or other worktrees; never stop/kill/restart v2 processes or anything you did not start; never bind 4103/4104/4113/4114 in tests.
- Every cargo command: `export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-web/target-track` and `flock /home/almalinux/repos/roost-v3-web/target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot <cmd>`. Never set RUSTFLAGS. Disk floor 10 GiB (`df --output=avail -B1G /home | tail -1`): below it, stop and report.
- You are a slice: ONE level deep. Never spawn agents. Never commit, never run `cargo fmt`, never push.
- NEVER end a turn while waiting on builds (the harness then forces a tool choice this model rejects → API 400 kills you). Block in the foreground: run the whole critical section as ONE foreground command, or `flock <lock> true` with timeout ≤3600, or an eval loop with `time.sleep`. Do not leave background services running.
- Files ≤400 lines incl. tests; no unwrap outside tests; `#[ignore]` only `#[ignore = "<slice>: <what>"]`.

## Where
Mutation worktree: `/home/almalinux/repos/roost-v3-web/target-track/dx-tree` (detached at snapshot `5a43d383`, identical to the lead's tree). Run cargo with `cwd` = that directory. NEVER edit `/home/almalinux/repos/roost-v3-web/crates/**` (the lead is committing from it).

Three agents share dx-tree. Every mutant must exist ONLY inside one critical section held under
`flock /home/almalinux/repos/roost-v3-web/target-track/.mut.lock`:
back up the product files you mutate (copy to `target-track/tmp/mut-<you>/`), apply the batch, run ONE
`cargo test -p <crate> [--features roost-web/smoke] --no-fail-fast --test <t1> --test <t2> …` (plus `--lib` if an inline test guards it),
capture the output to a file, restore the backups, then `git -C dx-tree diff --stat -- <product files>` must be empty
before the lock is released. Write it as one script (python3 or bash with `trap` restore) run in the foreground.

## Method
- For each NEW test function in your files (list below), pick the product line it guards and write a mutant that a
  plausible bug would produce (flip a comparison, drop a clear, off-by-one, swap an arm, return early). Batch up to ~10
  mutants per critical section, each in a different function, each expected to fail a named test; a batch whose mutants
  interfere is wasted, so keep them independent.
- A test that is a pure fixture/wiring check with no product invariant: say so and skip it (the lead may delete it).
- A mutant that SURVIVES (its test still passes) = an unguarded invariant: strengthen the test (edit only the test files
  you own, in dx-tree), re-run with the mutant → red, without → green. Product code is never changed.
- Do not "fix" anything else. If an existing test fails WITHOUT a mutant, stop and report it.

## Output (yield `data`)
`{ "mutations": [{"slice","file","line","change","expected_test","result":"failed"|"survived→strengthened"|"survived"}],
   "test_patch": "<abs path of git diff of your test-file edits in dx-tree, or null>",
   "skipped": [{"test","why"}], "notes": [] }`
Write the patch with `git -C /home/almalinux/repos/roost-v3-web/target-track/dx-tree diff -- <your test files> > /home/almalinux/repos/roost-v3-web/target-track/tmp/mut-<you>.patch`.
