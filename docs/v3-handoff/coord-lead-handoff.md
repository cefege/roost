# Coord lead handoff — Stage 2C closed, rows at 0

**A moment, not a state.** Read `git status --porcelain` and `git log --oneline -20`
in the coord worktree before trusting any line below. The plan
(`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2C") wins.

## Build environment (host `/home/mike/repos`)

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 \
       CARGO_TARGET_DIR=<worktree>/target-track
flock <worktree>/target-track/.roost-build.lock /home/mike/repos/roost-build-slot cargo …
```

Never end a turn to wait (API 400 crash); block in the foreground. Path-restricted
`git add` only (git identity is set in the repo config). JS: `bun install`;
`apps/web/dist` must be a SMOKE build (`VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`,
untracked). One spec against the Rust coord: `ROOST_SMOKE_COORD_EXECUTABLE=$PWD/target-track/debug/roost
ROOST_TEST_BUN=$(command -v bun) bun node_modules/@playwright/test/cli.js test
--config=playwright.config.ts --project=chromium-desktop --workers=1 smoke/terminal/<spec>`
(`@serial` specs such as terminal-local-fast-path need `--project=chromium-serial`).
Copy the `roost` binary aside before a spec if builds may run meanwhile; a copy
prints "is not a build of <worktree>; using it as given" and runs anyway.
After every track gate run `/home/mike/repos/roost-target-sweep <target-track>` under the
build lock.

Trap: a build-script binary compiled in another checkout path that shared this
target dir bakes that path in (`roost-proto` build.rs `env!("CARGO_MANIFEST_DIR")`);
if a build panics reading `/home/mike/repos/roost-v3-coord-verify/...`, delete
`target-track/debug/{build,.fingerprint}/roost-proto-<hash>` under the lock.

## RED AT HEAD, AND IT IS NOT NEW: `terminal_input_sync` 3/4

`cargo test -p roost-coord --no-fail-fast` = **1088 passed, 3 failed**, both runs
agreeing, all three in `tests/terminal_input_sync.rs`:
`a_probe_of_a_worker_outside_the_socket_scope_is_answered_empty`,
`a_session_outside_the_socket_scope_is_refused_for_input_and_claims` and
`a_claimed_route_is_answered_and_retired_when_its_socket_closes`. Each socket is
closed `1008 "invalid sync ack"` where a binary frame was expected
(`sync_ws_socket_support/mod.rs:195`).

NOT CAUSED BY THIS SESSION'S COMMITS: checked out at `de213b71`, before
`d6395a97` touched the ingress, the same three fail identically (1 passed,
3 failed, 5.68s). So the cause is `367c139d` or older and the open question is
which check refuses those frames -- which is exactly what `d6395a97` now answers
from the log: run `--test terminal_input_sync` with the refusal line visible and
read `path`, `ack_delivery_seq`, `last_sent_seq` and `frame_hex`.

## Committed on v3-coord

Wave 3 (`14f076df` … `90a293a1`): cherry-picks `5208e0f9`/`7a46b99a`, RenderHistory
stream id, `09f1b60c` fleet_update → roost-protocol (integrator cherry-pick candidate),
C-RETAIN, C-DIRECT, AT, GS, D1.

Wave 4: `f83fc63e` rustfmt+clippy, `cb49dd18` AG2 SessionsPrompt, `57ed3b36` C-PUSH,
`84467999` X2 DiagSnapshot.

Wave 5 (this session), all pushed:

| SHA | What |
|---|---|
| `367c139d` | sync_ws canonical client-frame check in field-number order — this is what fixed terminal-peer `:209` |
| `48b40471` | protocol: the capture command/result/section types (cherry-pick of worker `7a46b99a`) |
| `de213b71` | C-CAPTURE: `terminal_capture/` (bridge, lease, recorder, freeze, session_scope, worker_call) + wiring + 26 ported tests |
| `d6395a97` | sync_ws: every client-frame refusal names the check that produced it |
| `365ff392` | header audit: the eight v2 modules no header named, plus the `workers/worker-service.ts` README row |

**Ratchets, re-measured on this tree:**

- `grep -c 'PortStatus::AwaitingDomainPort' crates/roost-coord/src/rpc/method_route_rows.rs` = **0**.
- `#[ignore]` anywhere under `crates/roost-coord/` = **0**.
- Header audit: **204 v2 `apps/coord/src` non-test modules, 0 unaccounted** — each is
  named by a Rust `//!` header or listed in `crates/roost-coord/README.md`. The
  matcher accepts a bare stem, a brace form (`worker-send-attachment-{grant,peer,status}.ts`)
  and a suffix a sibling introduces (``-direct-terminal.ts``).

## Spec evidence (Rust coord debug build, TypeScript worker, smoke dist)

- `terminal-peer.spec.ts` **4/5**: `:96`, `:175`, `:209`, `:298` pass; `:61` fails
  `goto: Download is starting` on the worker's loopback door, identically with the
  TypeScript coordinator (environment, not the port).
- `terminal-peer.spec.ts:209` — the test this doc used to call an open defect — now
  passes, and was run twice: once alone with `-g "disabled peer capability"` (1 passed,
  5.0s) and once inside the full file. **There is no open Sync-resume defect.** The
  handoff line that called it one was written against a build at `84467999`, before
  `367c139d`; that commit's own body records the fix and the same spec result.
- earlier: terminal-render 5/5, attachment-direct 3/3, global-search 1/1.
- terminal-local-fast-path: see the report; not run in this session's budget window.

## What `d6395a97` changed, and why it is not scope creep

Every `1008` a Sync socket closes for a client frame logged the same reason,
`invalid_client_frame`, and five checks produce it. `InvalidFrame` gives each its own
reason, and the refusal line now carries the client's socket id, its acknowledgement,
the socket's last sent and highest acknowledged sequences, and the frame's bytes as
bounded hex. The close code and its wire reason are unchanged. Before this change,
`367c139d` had to be diagnosed from captured bytes by hand; the next occurrence reads
off the log.

## Open items, in plan order

1. `terminal_input_sync`'s three red cases (above): which check refuses those
   client frames. `d6395a97` names it in the log; nothing else is blocked on it.
2. `terminal-local-fast-path.spec.ts` on `--project=chromium-serial` — not run in this
   session (budget); the other four `terminal-peer` cases and `terminal-render` are green.
3. `terminal-peer :61` — environment (`goto: Download is starting`), identical with the
   TS coordinator; not a port defect and not this track's to fix.
4. The integrator owns the merge, the workspace gate and the release runbook.

## Next steps

1. Cherry-pick or merge `v3-coord` (tip `365ff392`) into `v3`.
2. Re-run the workspace gate on the merged tree; the coord half of it is green here.
3. `live-stack` READY check and the cutover runbook are the integrator's.
