# Integrator state — Roost v3

**Written 2026-09-27. Read this BEFORE any status report.** The failure mode it
exists to prevent: naming finished agents as live, and quoting a figure from a
tree that has moved. Both happened repeatedly tonight.

## Agent broker — MEASURED 2026-09-27, broker is back

`proc://` reopened after being down. Liveness below is from that read, not from
memory. Re-read it before asserting anything about a running agent.

**Live (3):** `CoordR3` up 1h4m · `CliCutover` up 4h10m · `WebLead4` up 50m.
**Finished:** `CoreSplits`, `WorkerRoot`, `CoordSplitFix`, `CoordGuardMove`,
`WebLead3`, `WorkerLead3`.

An earlier report of mine listed **four** live and had `CoreSplits` landing its
last splits. Both wrong — `CoreSplits` had already delivered. The error is
naming work in flight that the record says is finished.


## Branch heads — MEASURED 2026-09-27

| Branch | Worktree | Head |
|---|---|---|
| `v3` | roost-v3 | **`9bac9088`**, clean — one docs commit ON TOP of the CLI merge `dbf0edd2`, which is confirmed an ancestor |
| `v3-web` | roost-v3-web | `a9a7920e` (includes the coresplit merge) |
| `v3-worker` | roost-v3-worker | `57bd7f74`, clean |
| `coord-guard-move` | roost-v3-coord-split | `f7ff7e37` — **0 tracked modifications; merge is NOT blocked** |
| `v3-coord-split` | ref only | `e8f289c2` — already an ancestor of `coord-guard-move`; NOT pending |
| `v3-cli-cutover` | merged | `74991d26` — in `v3` via `dbf0edd2`; do not re-merge |


## Merged into v3

- `v3-cli-cutover` @ `dbf0edd2`. Gate: 404/0/0 twice, clippy 0, fmt clean, lint 0
  under `crates/roost-cli`, `bash -n join.sh` OK. Closes the Stage 4.5 enrolment
  blocker.
- Verified in the merged tree: `join.sh` pins `roost/v3/join.sh` twice, contains
  **0** occurrences of verified / authentic / trusted, honours
  `ROOST_RELEASE_BASE_URL`.

## Pending merges

1. **`coord-guard-move` → `v3-coord`** — after `CoordR3` reports. Both touch
   `roost-coord`. Do not merge into a worktree with a live agent in it.
2. **`v3-coord` → `v3`** — then the coord ratchets and clippy must be re-measured
   on the merged tree, not carried.
3. **`v3-web-coresplit` → `v3-web`** — already merged; `f9b0b2d8`'s
   `pub use report::{EchoPaint, PredictedCell};` is load-bearing for two
   web-terminal callers.

## BLOCKER — do not implement 2W-BOOT as written

`KeeperPool::channel_history` (`roost-worker/src/keeper_pool/session_seam.rs:83`)
is a **hard refusal against production**, naming `NO_REPORTED_HEAD` and
`NO_REPORTED_BASE_GEOMETRY`. Therefore:

- `SessionManager::adopt_survivor` **can never succeed** against a real keeper;
- its refusal path calls `abandon`, which **kills the survivor**.

So "call `adopt_survivor` on boot", which the plan's 2W-BOOT instructs,
**destroys every live terminal on every restart** — including the v2 keeper's PTYs
during the cutover. Any boot adoption must be gated on a replayability probe, and
the commit body must say the gate exists and why.

Second blocker from the same report: `ChannelDelivery` has **no production impl**
anywhere in `src/` (only `tests/session_support/fakes.rs`), and
`SessionManager::new` takes **two** slots — `cells: Arc<Mutex<dyn CellDelivery>>`
and `ingest: Arc<Mutex<dyn ChannelDelivery>>`. The root cannot construct a
`SessionManager` at all today. The shape is one type over one
`Arc<Mutex<CellEmitter>>` implementing both traits.

`WorkerRoot`'s work was **reverted, not delivered**; the tree is clean at
`57bd7f74` and the slice did not compile. Its twelve outstanding errors were all
mechanical and are enumerated in `agent://WorkerRoot`.

### That work IS recoverable — from the transcript, not from git

An earlier revision of this file said "unrecoverable". **That was wrong**, and
the shape of the mistake is worth keeping because it recurs.

Git genuinely holds nothing: `runtime/adoption.rs` is absent from the worktree,
no stash holds it, the reflog's newest entry is the commit at `57bd7f74`, and
`git fsck --unreachable` reports five dangling commits of which **none contains
it** — the two on `v3-worker` are WIP from `44c7af95` and `8a85f523`, long
behind `57bd7f74`.

**But the tool-call arguments are in the transcript.** Every `write`/`edit`
payload is a whole file body or a hashline patch, and all of them are there:
`~/.omp/agent/sessions/-repos-roost/2026-09-27T16-00-04-645Z_01a0e398-4e25-746d-b266-c869cb84d737/WorkerRoot.jsonl`

**Replay manifest: `/tmp/workerroot-replay.txt` — 64 operations in TRUE
TRANSCRIPT ORDER**, each tagged with its jsonl line.

**Order is load-bearing and my first version got it wrong.** I sorted the
manifest "writes first, then edits" for readability. Every edit carries a hash
tag anchoring it to the file *as of that moment*, so reordering breaks all 45
of them. Do not re-sort it.

| File | write | edit | shell |
|---|---|---|---|
| `runtime/boot_sequence.rs` | **0** | 17 | **0** |
| `runtime/mod.rs` | 1 | 13 | 0 |
| `runtime/session_stack.rs` | 1 | 6 | 0 |
| `runtime/reconcile.rs` | 0 | 7 | 0 |
| `runtime/channel_delivery.rs` | 1 | 2 | 0 | the `ChannelDelivery` production impl |
| `runtime/cell_delivery.rs` | 1 | 1 | 0 |
| `runtime/adoption.rs` | 1 | 0 | 0 | 10,799 chars, whole file |
| `runtime/door_serve.rs` | 1 | 0 | 0 | 9,816 chars |
| `runtime/capabilities.rs` | 1 | 0 | 0 | 5,454 chars |
| `runtime/link_serve.rs` | 0 | 1 | 0 | the `:113` comment fix; file pre-exists |
| *(shell steps)* | — | — | 9 | see below |

**`boot_sequence.rs` has no `write` — its base came from the shell**, so the 17
hash-anchored edits cannot apply until those steps have run:

```
sed -n '93,445p' mod.rs > /tmp/boot_body.rs ; sed -n '1,92p' mod.rs > /tmp/head.rs
sed -n '94,445p' /tmp/boot_body.rs; } > boot_sequence.rs     # concatenation
head -319 boot_sequence.rs > /tmp/bs_head.rs ; cp /tmp/bs_head.rs boot_sequence.rs
```

The last shell step in the transcript is the discard:
`git checkout -- crates/roost-worker/src/runtime/ && rm -…`. **Do not run it.**

`reconcile.rs` and `link_serve.rs` also have no write, but both pre-exist at
`57bd7f74`, so their edits anchor correctly.


So the rebuild is **a replay plus the twelve enumerated mechanical errors in
`agent://WorkerRoot` — not a redesign.**

**Checked, and the replay does NOT close the blocker.** The manifest's
`adoption.rs` routes every refusal into `adopted.unreplayable += 1` — it
*counts* the refusals, and its own header says so. It does not gate them,
because the kill is one level down and unchanged: `session/resume.rs` still has
three `self.abandon(…)` calls on its refusal paths (`:225`, `:232`, `:251`),
and `abandon` calls `self.keeper.kill_channel(channel)` (`:309`).

So the replay lands the door, the session stack, the two delivery impls and the
capability list — and the kill is still armed behind them. The gate has to be
written **before** that code reaches a boot path, not after.

Three method notes, all from getting this wrong first:

- The transcript is JSONL with **three different tool-call shapes**. Reading one
  of them returns "0 mutating calls" and looks like an answer. A recursive
  walker over every key name finds all **449** tool calls, of which 63 are
  write/edit — 55 with non-empty bodies, 64 once the 9 shell steps are counted.
- **Not every file change is a `write`/`edit`.** 212 bash calls were scanned for
  `sed -i`, `cat >`, `tee`, `perl -i`, `cp`, `mv`, `rm`: one real pipeline
  created `boot_sequence.rs`. A manifest built only from write/edit calls is
  silently incomplete.
- `git grep <sha>` measures a commit; a plain search of a worktree measures
  whatever is on disk. Same error class, different tool.

**When committing the replay:** branch it off `57bd7f74`, not on `v3-worker`;
do not build while the machine is busy; and the commit message must say the
slice **must not be merged as-is**, because it contains the boot-time
`adopt_survivor` call described below.

## S3.0 workspace gate

- Script: `/tmp/s3-workspace-gate.sh`. Four criteria; refuses with exit 75 when
  `pgrep -x cargo` is non-empty. **Refusal proven four times.**
- Smoke check, NOT a gate: `/tmp/s3-smoke-check.sh`.
- **Never run since the CLI merge.** First action:
  `cargo check -p roost-coord -p roost-worker -p roost-host --all-targets`.
- Reason it matters: the merge added `EnvSource: Sync` to `roost-host`, a shared
  crate. A `-p roost-cli` gate structurally cannot see a coord or worker break.
  Checked by grep on all three branches: 0 impls outside roost-host+roost-cli.
  Unproven by compilation.

## Figures, each with the tree it was measured on

| Figure | Tree |
|---|---|
| `AwaitingDomainPort` 27, `UnwiredInV2` 16, `UNFINISHED` 3, `todo!` 0 | `v3` @ `dbf0edd2` |
| worker `UNIMPLEMENTED`: 6 on `v3`, **2** on `v3-worker` @ `57bd7f74` | committed, via `git grep <sha>` |
| `roost-cli` gate 404/0/0 twice | `a86bc6d4` |

**Never quote a figure from a plain `grep` in a worktree with uncommitted
changes.** `WorkerRoot`'s tree read 1, then 0, then 2 while its HEAD stayed
`57bd7f74`. Use `git grep -c UNIMPLEMENTED <sha> -- <path>`, and note the output
is `sha:path:count` — sum the **last** field.

## Unowned / open

- `recover-held-rollout.ts`, untracked at the root of `roost-v3-coord-split`.
  Imports `./apps/roost-cli/src/…`, which only resolve in the v2 checkout. In no
  commit. **The user's call — do not delete.**
- `origin/v3-cli` is ungated and superseded. Worktree removed. **Deleting the
  remote branch is the user's call — not done.**
- `u4gate` service shows failed and is unidentifiable. Not a gate anyone awaits.
