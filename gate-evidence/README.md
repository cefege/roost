# Gate evidence

Raw `cargo test -p roost-cli --no-fail-fast` output for each run behind a
published figure. A figure with no run behind it is not a figure, and a
`git worktree remove` deleting the only copy of the run is how that happens.

**Current: `d5c828f9` — 399 passed / 0 failed / 0 ignored, twice.**
`gate-run-A-d5c828f9.log` and `gate-run-B-d5c828f9.log`. Two real runs, not one
run copied: different per-binary timings, different test execution order.

The other four criteria on the same SHA:

| Criterion | Command | Result |
| --- | --- | --- |
| tests (twice) | `cargo test -p roost-cli --no-fail-fast` | 399 / 0 / 0, twice |
| clippy | `cargo clippy -p roost-cli --all-targets -- -D warnings` | 0 errors |
| fmt | `cargo fmt -p roost-cli -- --check` | clean |
| lint | `cargo xtask lint` | 0 violations under `crates/roost-cli` |

`import-v2`, first execution ever: `import_v2_copy` 9/0 and `import_v2_plan`
10/0 — **19 passed, 0 failed**.

## Superseded

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
