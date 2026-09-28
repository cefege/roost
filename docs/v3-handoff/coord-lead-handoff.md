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

## The `terminal_input_sync` reds: a FIXTURE defect, found and fixed (1888d9c5)

Three of that binary's four cases had been red on every run since `367c139d`, each
socket closed `1008 "invalid sync ack"` where the harness expected a binary frame.
The coordinator was right; the harness was wrong.

`send_client_frame` built its bytes with buffa's `encode_to_vec`, which writes a
message's fields in DECLARATION order, and `SyncClientFrame` declares its `command`
oneof before `socket_id = 10` while `input_route_claim = 11` and
`terminal_transport_probe = 12` number after it. The three red cases are exactly the
ones that send those two commands, so their bytes put the oneof ahead of the socket
id — a frame no client can produce. Every real client encodes with protobuf-es, which
writes ascending field number, and v2 refuses a frame that is not the canonical
encoding of what it decoded to (`sync-ws-client-ingress.ts:41-45`), so v2 refuses the
same bytes. `tests/sync_client_frame_canonical.rs` already pinned both halves, with a
route claim captured from the smoke browser as the fixture.

The fix is the fixture's ENCODER (`tests/sync_ws_socket_support/canonical_bytes.rs`):
the same top-level fields in ascending field number. No accept condition widened, no
assert relaxed, no product line changed. `terminal_input_sync` is 4/4 (0.16s) with
every case's own assertions intact.

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
- `terminal-local-fast-path.spec.ts` on `--project=chromium-serial`: run, **1 failed**,
  and the failure is the environment, not the port — `goto: Download is starting` at
  `smoke/terminal/fixtures.ts:144`, the same enrollment navigation that reds
  `terminal-peer :61`. Re-run with `ROOST_SMOKE_COORD_EXECUTABLE` unset (the
  TypeScript coordinator) it fails identically, which is the classification.

## What `d6395a97` changed, and why it is not scope creep

Every `1008` a Sync socket closes for a client frame logged the same reason,
`invalid_client_frame`, and five checks produce it. `InvalidFrame` gives each its own
reason, and the refusal line now carries the client's socket id, its acknowledgement,
the socket's last sent and highest acknowledged sequences, and the frame's bytes as
bounded hex. The close code and its wire reason are unchanged. Before this change,
`367c139d` had to be diagnosed from captured bytes by hand; the next occurrence reads
off the log.

## Gate (green, run twice)

```
cargo test -p roost-coord --no-fail-fast   194 binaries, 1092 passed, 0 failed  (twice, agreeing)
cargo clippy --workspace --all-targets -- -D warnings   exit 0
ROOST_REPO_ROOT=$PWD cargo xtask lint       0 violations, 3396 inputs
cargo xtask fmt                             exit 0, `git status --short` empty
```

## Open items, in plan order

1. `terminal-peer :61` and `terminal-local-fast-path`: both `goto: Download is
   starting` at the browser enrollment, both identical with the TypeScript
   coordinator. Host environment; not a port defect and not this track's to fix.
2. The integrator owns the merge, the single workspace gate, the `live-stack` READY
   check and the release runbook.

## Next steps

1. Merge or cherry-pick `v3-coord` (tip below) into `v3`; `de213b71` needs `48b40471`
   before it, so take both in order.
2. Run the ONE workspace gate on the merged tree. Every coord-side command in it is
   green on this branch, except the two environment-blocked specs above.
