# Track C — C2 and C3 delegation context (CoordLeadC2-2)

Worktree `/home/almalinux/repos/roost-v3-coord`, branch `v3-coord`. **All paths
absolute and inside that worktree.** `xtask/`, `crates/roost-protocol`,
`crates/roost-proto`, `smoke/`, `docs/v3-wave-gate.md` and the phase gates are
**integrator-owned — do not edit them.** `docs/phase3-coord-contract.md` is the
contract; read it, do not write it.

## Build environment — the only correct one

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
       CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-coord/target-track
```

**One compiler per target dir, and the LEAD runs it.** Slice agents edit and
report; the lead compiles and routes diagnostics by file. Do not start
`cargo check`/`test`/`clippy` in this worktree — you will contend on the build
lock and be reported as a phantom failure.

## The rules that decide whether your work survives

- **Commit at every stage boundary, before a long build.** Incompleteness is the
  reason to commit, not the reason to wait. A preservation commit whose body
  states what is verified and what is not is the correct artefact.
- **≤400 lines per file including inline `#[cfg(test)]`.** `//!` headers 3–6
  lines. No `macro_rules!`. No top-level mutable state. No `unwrap`/`expect`
  outside tests. No `todo!()`, no `UNIMPLEMENTED:`, no stubs.
- **A cross-module move has three parts — the new file, the cut, and the
  declaration — and the imports each half stops using.** A commit with two of
  the three is a broken tree wearing a preservation commit's body. That has
  happened four times on this track.
- **A linter's suggested fix is a HYPOTHESIS.** Read the comment above the line
  first. Clippy's `.all()` suggestion on `tests/agent_status_wait.rs` was
  applied and the next run disagreed, because `.all()` short-circuits and the
  test registers sessions until a bound refuses one — the short-circuit stops at
  the first refusal and the bound is never reached. Where a compiler offers two
  fixes the **primary** is more often the diagnosis.
- **A short-circuiting operator whose operands MUTATE is a defect class no gate
  catches.** `||` stops at the first truthy term, so a `clear_all` used as an
  operand is *conditionally executed*.
- **A mutation site whose value the TEST READS is not a lever.** Ask what the
  test consumes before choosing a site.
- **Point a mutation at a `file:line`, never a pattern.** One CLI file has a
  correct decoy at `:127` that a pattern-matched mutation hits instead.
- **A zero from a search is only a fact with a stated reason for the zero.**
  "No callers" is an instrument reading, not a measurement.

## Baseline — measured, and every figure carries the mechanism that bounded it

| Claim | Figure | Bounded by |
|---|---|---|
| `cargo test -p roost-coord --no-fail-fast` run 1 on `26782410` | **618 passed / 1 failed / 3 ignored, 97 binaries** (96 `ok`, 1 `FAILED`) | one uncontended run on a committed tree; 97 `Running` lines reconcile to 622 = 618+1+3 |
| run 2 on the C5 tip | see the lead's report — two agreeing runs is the C5 gate | |
| `cargo clippy -p roost-coord --all-targets -- -D warnings` run 12 | **exit 0**, `Finished` 2m06s | **nothing** — it reached every target and emitted nothing, so a TOTAL, not a floor. **Runs 6, 7, 8 were CONTAMINATED, not floors**: all three died with `couldn't read .../out/private.rs` from the retired disk guard, which means the run was damaged by its own primer. |
| `cargo check -p roost-coord --all-targets --keep-going` | **exit 0** | `--keep-going` is a total |
| `cargo test -p roost-coord --test agent_status_rpc` | **9 passed / 0 failed** | first run ever over the shared `roost_protocol::wire::agent_status::AgentStatusOrder` |

Target list is exactly lib + **95 test binaries**: no `[[bin]]`, no examples, no
explicit `[[test]]` in `crates/roost-coord/Cargo.toml`, no `src/bin` or
`examples/`. A `.rs` file count is NOT a binary count — the `*_support/`
modules compile *into* a binary rather than being one.

The single held red is `mcp_relays_authority::a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout`
— the port answers `Internal` where v2 answers `Unavailable`, which changes
client retry behaviour. **NOT OURS TO DECIDE.** Not fixed, not hidden, not
ignored.

## House rules for new `pub`

> **A COMPILATION UNIT carries `#![allow(clippy::unwrap_used, clippy::expect_used)]`
> if any file in it has an expect/unwrap outside a `#[test]` body.** For a test
> binary that is the root; for a shared fixture, every consumer declares it or
> the fixture declares itself.

Verified over every coord fixture by consumer: **zero gaps.** The old "62 sites"
figure was a miscount — it counted *occurrences* and the deciding question was
never about files.

## The one agent-status pin — one test, not two

`tests/agent_status_rpc.rs:191` asserts at `:224-227` that a legacy-held session
refuses an identified report at **revision 6**, which trips the guard at
`crates/roost-protocol/src/wire/agent_status/order.rs:191` (`held` is
unidentified **and** `revision > 1`). A candidate sending revision 1 satisfies
neither clause and passes either way. **One test is the whole coverage.**

## Gate commands the lead runs

`cargo check -p roost-coord --all-targets --keep-going` ·
`cargo test -p roost-coord --no-fail-fast` ·
`cargo clippy -p roost-coord --all-targets -- -D warnings` ·
`ROOST_REPO_ROOT=/home/almalinux/repos/roost-v3-coord cargo run -p xtask -- lint` ·
`cargo fmt --check`
