# Fix: `roost-cli`'s signal test SIGINTs every process the user owns

## Context

The whole omp session tree (Main plus every track lead) keeps dying at once. It happened at 02:25:15, 03:08:02, 08:31:30 and 09:35:13 on 2026-09-27. Each omp log records `"Session exit recorded" … "reason":"sigint","kind":"signal"`. At the same second, the system journal shows `systemd[<user manager>]: Received SIGINT from PID N (kill)`, followed by the user manager exiting (`Activating special unit Exit the Session`). That stops `roost-worker.service`, and the omp session runs inside a Roost terminal hosted by that service (`omp → bash --rcfile /tmp/roost-bash-osc7/roost.bashrc → RoostWorkerV2 bun → systemd --user`). `paul-roost-origin-tunnel.service` and `wireplumber` died of SIGINT in the same second. The only thing those processes have in common is that the same user owns them, so this is a `kill(-1, SIGINT)` broadcast. It is not an OOM kill (the kernel log has none) and not a crash inside omp.

Source: `crates/roost-cli/src/dev/signal.rs` on branch `v3-cli` (worktree `/home/almalinux/repos/roost-v3-cli`, introduced in `8e3dc823`). Its unit test calls `send(u32::MAX, INTERRUPT)`, which runs `kill -INT 4294967295`. util-linux `kill` 2.37.4 (`/usr/bin/kill`) parses that into a signed 32-bit `pid_t`, so it becomes `-1`, meaning "every process I may signal". Each of the four shutdowns came 100–233 s after a `cargo test -p roost-cli --no-fail-fast` on that worktree, which is the time needed to reach the lib unit tests. The test exists only on `v3-cli`; `v3`, `v3-worker`, `v3-web` and `v3-coord` do not have `signal.rs`.

End state: `send` refuses any pid that `kill` would read as a process group or a broadcast, before spawning anything. The dangerous test is replaced by tests that cannot broadcast. The fix is committed and pushed on `v3-cli`, and a `roost-cli` test run no longer takes the user session down.

## Approach

### 1. Stop anyone from re-triggering it before the fix lands

Before editing, send this to `agent://all` with `write`. A "no live peers" result is fine; continue either way.

> STOP: do not run `cargo test -p roost-cli` (any filter, including `--lib`) or `cargo test --workspace` in `/home/almalinux/repos/roost-v3-cli` until commit "cli: a u32::MAX pid made the signal test SIGINT every process the user owns" is pushed on `v3-cli`. `src/dev/signal.rs`'s unit test runs `kill -INT 4294967295`, which util-linux reads as `kill(-1)` and which SIGINTs every process this user owns: the systemd user manager, the Roost worker hosting our terminals, and every omp session. That is what killed all sessions at 02:25, 03:08, 08:31 and 09:35. The integrator is editing ONLY `crates/roost-cli/src/dev/signal.rs` in that worktree; do not touch that file.

### 2. Reproduce the broadcast safely, inside a rootless PID namespace

`unshare -Urpf --mount-proc` works on this host (probe printed `inside pid=1`, with 3 processes visible), and `kill(-1)` inside it reaches only processes in that namespace. Run the old test there with a sentinel process that resets SIGINT to its default action (a plain background `sleep` in `sh -c` inherits SIGINT-ignored and would prove nothing).

Working dir `/home/almalinux/repos/roost-v3-cli`, before any edit:

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-cli/target-track
cargo test -p roost-cli --lib --no-run 2>&1 | grep -o 'target-track/debug/deps/roost_cli-[0-9a-f]*' | tail -1
# BIN=<the absolute path printed above, prefixed with /home/almalinux/repos/roost-v3-cli/>
unshare -Urpf --mount-proc sh -c '
  python3 -c "import signal,time; signal.signal(signal.SIGINT, signal.SIG_DFL); time.sleep(300)" &
  S=$!; sleep 0.5
  "$0" dev::signal; echo "TEST_EXIT=$?"
  sleep 0.5; kill -0 $S 2>/dev/null && echo SENTINEL_ALIVE || echo SENTINEL_KILLED' "$BIN"
```

Expected before the fix: `SENTINEL_KILLED`, and `TEST_EXIT` is non-zero (130, or the harness's signal status), because the test binary is also hit. Note that `--no-run` prints `Executable unittests src/lib.rs (...)`; the grep extracts the path. If it prints nothing, the lib test target failed to build: run `cargo test -p roost-cli --lib --no-run` without the grep, fix nothing unrelated, and report the compile error instead of continuing.

### 3. Guard `send` against group and broadcast pids

Edit `crates/roost-cli/src/dev/signal.rs`. No other caller needs to change: `supervisor.rs:219` `signal_the_live` already has a catch-all `Err(failure) => tracing::error!(…)` arm, which is the right treatment for the new variant. Its pids come from `Child::id()` and are always valid.

- Add a first variant to `SignalError`:
  ```rust
  #[error("pid {pid} names no single process: `kill` would read it as a process group or as every process")]
  NotASingleProcess { pid: u32 },
  ```
- In `send`, before building the `Command`, return early:
  ```rust
  // `kill` parses its operand into a signed pid_t: 0 is the caller's own
  // process group, and anything above i32::MAX wraps negative — u32::MAX
  // becomes -1, which is every process this user may signal.
  if pid == 0 || i32::try_from(pid).is_err() {
      return Err(SignalError::NotASingleProcess { pid });
  }
  ```
- Update the `send` doc comment (currently lines 28–30) to add one sentence: a pid `kill` would read as a group or as every process is refused before anything is sent.
- Replace the whole `#[cfg(test)] mod tests` block with:
  ```rust
  #[cfg(test)]
  mod tests {
      use super::{INTERRUPT, SignalError, send};

      #[test]
      fn a_pid_kill_would_read_as_a_group_or_as_everyone_is_refused_before_anything_is_sent() {
          for pid in [0, u32::MAX, 1 << 31] {
              let outcome = send(pid, INTERRUPT);
              assert!(
                  matches!(outcome, Err(SignalError::NotASingleProcess { pid: refused }) if refused == pid),
                  "pid {pid} reached the kill program: {outcome:?}"
              );
          }
      }

      #[cfg(target_os = "linux")]
      #[test]
      fn a_pid_that_no_longer_exists_is_refused_rather_than_reported_as_sent() {
          // Linux never hands out a pid at or above pid_max, so this one names
          // no process and the kernel refuses it with ESRCH.
          let pid_max: u32 = std::fs::read_to_string("/proc/sys/kernel/pid_max")
              .expect("pid_max is readable")
              .trim()
              .parse()
              .expect("pid_max is a number");
          assert!(matches!(send(pid_max, INTERRUPT), Err(SignalError::Refused { .. })));
      }
  }
  ```
  The `expect`s sit inside `#[cfg(test)]`, which `allow-unwrap-in-tests` / `allow-expect-in-tests` already exempt, so no `#![allow]` is needed. The existence test is Linux-only because macOS has no `/proc`; the guard test is portable.

`status/service_probe.rs` also shells out to `kill`, but only with `child.id()` of a child it spawned, so it gets no change.

### 4. Commit and push on `v3-cli`

In `/home/almalinux/repos/roost-v3-cli`, stage only `crates/roost-cli/src/dev/signal.rs` (`git add crates/roost-cli/src/dev/signal.rs`; do not `git add -A`, because the track lead may have uncommitted work). Commit:

```
cli: a u32::MAX pid made the signal test SIGINT every process the user owns

`kill -INT 4294967295` is `kill(-1, SIGINT)` once util-linux parses it into
a signed pid_t. The unit test in dev/signal.rs sent exactly that, so every
`cargo test -p roost-cli` SIGINTed the systemd user manager, the Roost
worker hosting the terminals, and every agent session on the host.
`send` now refuses 0 and anything above i32::MAX before spawning `kill`,
and the existence test uses pid_max, which Linux never allocates.
```

Then `git push origin v3-cli`. If the push is rejected as non-fast-forward, run `git pull --no-rebase origin v3-cli`, resolve nothing outside `signal.rs` (abort and report if the conflict is elsewhere), re-run step 5's namespace check, and push again.

### 5. Tell the tracks it is safe again

`write agent://all`: "Fixed and pushed as <sha> on v3-cli: `roost-cli` tests are safe to run again. Cause: `send(u32::MAX, …)` → `kill -INT 4294967295` → `kill(-1)`. Any interrupted `cargo test -p roost-cli` run (exit 130) was this, not a finding against the code: mark it INTERRUPTED and re-run."

## Critical files & anchors

- `/home/almalinux/repos/roost-v3-cli/crates/roost-cli/src/dev/signal.rs`: `send` (line ~31) and the `tests` module (line ~43), the source of the broadcast.
- `/home/almalinux/repos/roost-v3-cli/crates/roost-cli/src/dev/supervisor.rs`: `signal_the_live` (line ~214), the only production caller. Its catch-all arm absorbs the new variant; no edit.

## Verification

1. **Before/after in the namespace (proves the fix without risking the session).** Re-run the exact step 2 command after step 3, rebuilding with the same `--no-run` line first (the binary hash changes; use the new path). Expected: `test result: ok. 2 passed` (Linux), `TEST_EXIT=0`, and `SENTINEL_ALIVE`. Before the fix it was `SENTINEL_KILLED`.
2. **The real run no longer kills the session.** Record `START=$(date -u +'%Y-%m-%d %H:%M:%S')`, then outside any namespace, in `/home/almalinux/repos/roost-v3-cli` with step 2's env:
   `cargo test -p roost-cli --no-fail-fast 2>&1 | grep -E '^test result|^error' | sort | uniq -c`
   Then `journalctl --since "$START" --no-pager | grep -E 'Received SIGINT from PID|Exit the Session'` must print nothing, and `systemctl --user is-active roost-worker roost-coord omp-auth-broker` must print `active` three times. The suite's own pass/fail counts are not part of this fix's acceptance: its other failures belong to the CLI track. This fix is accepted when no broadcast happens and `dev::signal`'s two tests pass.
3. **Nothing else can broadcast.** `grep -rnE 'Command::new\("kill"\)' /home/almalinux/repos/roost-v3*/crates --include=*.rs` must show only `dev/signal.rs`, `status/service_probe.rs` (pid from `child.id()`), and `tests/dev_fan_out.rs:128` (pid from `std::process::id()`). Any other hit must be read to confirm its pid comes from a live `Child` or `std::process::id()`. If one takes a constant or parsed pid, route it through `dev::signal::send` so the guard covers it.

## Assumptions & contingencies

- If step 2 prints `SENTINEL_ALIVE` before the fix (the broadcast does not reproduce), do not change the diagnosis on that basis alone. First confirm the sentinel line ran (`python3` present) and that `TEST_EXIT` shows the test executed. The fix is still correct and still applied, because `u32::MAX` → `-1` is the documented `pid_t` conversion. Report the non-reproduction in the step 5 message.
- If `systemctl --user is-active` shows any of the three services inactive after verification 2 while the journal shows no SIGINT, start them with `systemctl --user start roost-worker roost-coord omp-auth-broker` and report it. That would be a separate problem from this fix.
