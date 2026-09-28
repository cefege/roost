# Coord lead handoff — Stage 2C (sockets live, then the rows)

**A moment, not a state.** Read `git status --porcelain` and `git log --oneline -15`
in `/home/almalinux/repos/roost-v3-coord` before trusting any line below. The plan
(`docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` "### Stage 2C") wins.

## Build environment

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=3 \
       CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-coord/target-track
flock target-track/.roost-build.lock /home/almalinux/repos/roost-build-slot cargo …
```

Never end a turn to wait (API 400 crash); block in the foreground. Every commit
uses a path-restricted `git add` (slices share the tree). JS: `bun install` done;
`apps/web/dist` is a SMOKE build (`VITE_ROOST_SMOKE=1 bun run --cwd apps/web build`),
untracked. One spec against the Rust coord: `ROOST_SMOKE_COORD_EXECUTABLE=$PWD/target-track/debug/roost
ROOST_TEST_BUN=$(command -v bun) bun node_modules/@playwright/test/cli.js test
--config=playwright.config.ts --project=chromium-desktop --workers=1 smoke/terminal/<spec>`.
After every track gate: `/home/almalinux/repos/roost-target-sweep <target-track>` under the
build lock; `cargo clean` between waves when target-track > ~12 GiB; delete release dirs
after live-stack checks.

## Done (SHAs, all pushed to origin/v3-coord)

| SHA | What |
|---|---|
| `3ab53db1`, `e316737f` | merges of worker `29efc28e` (view registry → roost-protocol) and `deda6301` |
| `466d74d8` | C-BOOT: authorized-keys import at boot, FK validation, pre-migration backup |
| `7ff8f75e` | **C-B**: WL-WIRE + SY2 + capability `terminal_metadata_v1` (compiles only with `6d72521c`) |
| `2327f074` | AuthRedeemWorker/Browser public as v2 (enrollment was broken) |
| `6d72521c` | S4 sessions: 7 rows (AwaitingDomainPort 19 → 12) |
| `a07f9ead`, `1fdc090a`, `b37b744b` | shared layout adapter cherry-picked from v3-web; coord's `ui_state/layout_proto.rs` deleted |
| `fa50bcf9` | clippy let_and_return |
| `638fb4dc` | handoff after C-B |
| `c9772f99` | protocol: KeeperContractV1 carries v2's `bun_abi`; keeper reports `KEEPER_RUNTIME_ABI` "rust" |
| `5d9d9001` | C-INPUT + C-SEND + SY3 + C-SCREEN (one commit); SessionsInput (12 → 11) |

C-B check (at `fa50bcf9`): release roost + live-stack printed `READY http://127.0.0.1:32921
worker=f9d8bb6d…` (TS worker, TS web). Gate at `5d9d9001`: roost-coord+roost-protocol 188
binaries 1247/0/0, clippy workspace 0, lint 3115 inputs 0 violations. Specs with the Rust
coord (chromium, workers=1): terminal-delivery 4/4, terminal-render 3/5 (history cases
"streaming sequence repair leaves an off-bottom reader fixed" and "main screen history
survives width and height perturbations" fail Rust-only; TS coord 5/5).

Ratchet at `5d9d9001`: AwaitingDomainPort **11**; `#[ignore]` 0. Coverage audit
(`/tmp/coordlead-coverage.sh`: v2 basename in a `//!` line or README): 64 uncovered.

## Open items carried

- terminal-render 2 history failures (screen/view resync path or Sync terminal lane).
- CScreen2 mutation batch C not run (`/tmp/cscreen2_batch_C.json`).
- terminal-input.spec.ts not yet run against the Rust coord.
- AG2/C-PUSH wait for the worker lead's agent_status `active=false` protocol fix SHA;
  C-CAPTURE should reuse the worker's roost-protocol terminal_capture types (WCapture).

## Next step

Wave 3: RenderHistory fix, C-DIRECT, AT, GS, D1, X2, C-RETAIN; then AG2, C-PUSH,
C-CAPTURE; then `crates/roost-coord/README.md` (not-ported-by-decision: v2
`_migrations` adoption; Windows update broker/deploy) + header audit to 0 uncovered.
