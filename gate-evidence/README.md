# Gate evidence

Raw `cargo test -p roost-cli --no-fail-fast` output for each run behind a
published figure. A figure with no run behind it is not a figure, and a
`git worktree remove` deleting the only copy of the run is how that happens.

`parity/` holds the Playwright JSON behind each parity verdict: a run record
`<stack>-<sha>.run.json` (written by `bun smoke/parity/run.ts suite`, its
`jsonPath`s pointing at the files beside it) and its `-main`/`-serial` reports.
`bun smoke/parity/run.ts verdict <rust.run.json> parity/bun-<sha>.run.json`
reads them directly. **Current Bun baseline: `bun-d5bd76c7`** — main 141 / 1 / 3,
serial 15 / 0 / 3 (passed / failed / skipped). **Current Rust final gate:
`rust-c1310f27`** — main 142 / 0 / 3, serial 15 / 0 / 3; against `bun-d5bd76c7`, gap 0,
rust-skip-only 0, both-skip 6. The flakes it does not settle are recorded in
`docs/v3-gate-baselines.md` under "All-Rust oracle runs, watched".

**Current: `a86bc6d4` — 404 passed / 0 failed / 0 ignored, twice.**
`gate-run-A-a86bc6d4.log` and `gate-run-B-a86bc6d4.log`. The five added tests are
`tests/join_script.rs`, which runs the real `join.sh` against fake `roost`
binaries: a pre-v3 binary on PATH is never exec'd, a v3 one at the self-link
location is, and the command that prints the script's URL and the script
itself name the same one.

Two real runs, not one run copied: different per-binary timings, different
test execution order.

The four criteria on `a86bc6d4`:

| Criterion | Command | Result |
| --- | --- | --- |
| tests (twice) | `cargo test -p roost-cli --no-fail-fast` | 404 / 0 / 0, twice |
| clippy | `cargo clippy -p roost-cli --all-targets -- -D warnings` | 0 errors |
| fmt | `cargo fmt -p roost-cli -- --check` | clean |
| lint | `cargo xtask lint` | 0 violations under `crates/roost-cli` |

`import-v2`, first execution ever: `import_v2_copy` 9/0 and `import_v2_plan`
10/0 — **19 passed, 0 failed**.

## Superseded

`2cdd0e04` — 399 / 0 / 0 twice, all four criteria green. This is the SHA
merged into `v3`; the two commits after it are the join change.

`d5c828f9` — 399 / 0 / 0 twice, clippy 0, fmt clean, lint 0, on all four
criteria. `crates/roost-cli` is byte-identical between `d5c828f9` and
`2cdd0e04`; the only change is this directory. Kept because the figure a branch
head carries and the figure `v3` carries should both be readable.

`a13c385d` — 399 / 0 / 0 twice (`gate-run-A-a13c385d.log`,
`gate-run-B-a13c385d.log`). Clippy was **red** on that tree: the splits left
unused imports behind and `-D warnings` found them. Kept because the sequence is
the record — the tests were green before the lint gate was, which is why a green
test figure alone never certified this branch.

## Known flake, not fixed by anything here

`dev_fan_out::a_server_that_cannot_start_names_itself_and_stops_what_already_ran`
is load-dependent and pre-existing. It failed on `fa61f851` (379/0 then 378/1),
passed twice under load on `a13c385d` and twice on `d5c828f9`, and passed 4/4 on
two isolated re-runs. **2L.1c's ported fix does not address it** — that fix
pins a precondition on `assert_stopped`, a different test. A green run here
means "passed twice under load at this SHA", not "fixed".
