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
uses a path-restricted `git add` (slices share the tree). JS side: `bun install` +
`bun run --cwd apps/web build` done once here (`apps/web/dist`, untracked).

## Done (SHAs, all pushed to origin/v3-coord)

| SHA | What |
|---|---|
| `3ab53db1`, `e316737f` | merges of worker `29efc28e` (view registry → roost-protocol) and `deda6301` |
| `466d74d8` | C-BOOT: authorized-keys import at boot, FK validation, pre-migration backup |
| `7ff8f75e` | **C-B**: WL-WIRE (worker link wire, lifecycle seam, v2 client_seq) + SY2 (sync socket/driver/v1 live) + capability `terminal_metadata_v1` |
| `2327f074` | AuthRedeemWorker/Browser public as v2 (enrollment was broken) |
| `6d72521c` | S4 sessions: 7 rows (AwaitingDomainPort 19 → 12) |
| `a07f9ead`, `1fdc090a`, `b37b744b` | shared layout adapter cherry-picked from v3-web; coord's `ui_state/layout_proto.rs` deleted |
| `fa50bcf9` | clippy let_and_return in a sessions test |

C-B check: release `roost` → `ROOST_SMOKE_COORD_EXECUTABLE=<release roost> bun
smoke/terminal/live-stack.ts` printed `READY http://127.0.0.1:32921 worker=f9d8bb6d…`
(TS worker, TS web) at `fa50bcf9`. Gate at that tree: roost-coord+roost-protocol
162 binaries 1120/0/0, clippy workspace 0, lint 2949 inputs 0 violations.

Ratchet at `fa50bcf9`: AwaitingDomainPort **12**; `#[ignore]` 0; `delegated_` 29.

## Visible refusals left for named slices

- C-INPUT: `sync_ws/ingress.rs::refuse_unrouted_terminal_command` (warn
  `terminal_command_unrouted`); `SessionsInput` row; `NoRouteRetirement`.
- SY3: `v1_seed_unported`, `domain_seed_unported`, `recovery_unported` warns.
- C-SCREEN: `LinkState.screen = NoTerminalSnapshotHub`; no views sink; lease-expiry 1013.
- Unassigned: live 4001 close of open Sync sockets on key revoke (v2 closeForFingerprint).

## Next step

Wave C-C/C-D first group: C-INPUT, C-SEND, SY3, C-SCREEN (prep map:
agent `CoordLead2C.CoordPrepScout2` report — shared seams + hot spots). Then
C-DIRECT, AT, AG2, X2, D1, GS, C-CAPTURE, C-PUSH, C-RETAIN in parallel. Then
`crates/roost-coord/README.md` (not-ported-by-decision: v2 `_migrations`
adoption; Windows update broker/deploy) and module-coverage audit.
