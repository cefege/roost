# Roost v3 — restart prompt for a fresh session on a new host

Paste everything below the line into a new agent session on the new machine.
It assumes the session knows nothing: no prior conversation, no `local://`
files, no running agents. Everything it needs is in the repository.

---

You are the **integrator** for the Roost v3 rewrite. Read this whole prompt
before running anything.

## 1. What Roost is and what we are doing

Roost is a self-hosted terminal-sharing product: a **coordinator** (auth,
sessions, sync fan-out, HTTPS/WebSocket front door), a **worker** per machine
(owns PTYs through a long-lived **keeper** process), a **web UI**, and a
**CLI** (`roost`). The shipping product ("v2") is TypeScript/Bun, on branch
`main`, under `apps/` and `packages/`. The rewrite ("v3") is Rust + Dioxus,
on branch `v3`, under `crates/`.

Goal: **full behavioural parity with v2**, then run production on v3. Parity
is judged by v2's own Playwright suite under `smoke/terminal/` (TS baseline
142 passed / 0 failed / 3 skipped correctness + 15/0/3 perf) and the in-scope
rows of `FEATURES/README.md`. **Parity rule: v2 behaviour wins** wherever the
port and v2 disagree, including v2 imperfections — with one approved
exception (see §7).

## 2. Repository and branches

Remote: `https://github.com/cefege/roost.git`.

| Role | Branch | Tip at pause | Uncommitted work (snapshot) → restore onto |
|---|---|---|---|
| integrator | `v3` | `origin/v3` (the commit that added this file) | none |
| coord track (Stage 2C) | `v3-coord` | `b7ad6997` | `v3-coord-snap-pause` = `6c6245e4` → `b7ad6997` |
| worker track (Stage 2W) | `recover/workerroot` (always also pushed to `v3-worker`) | `0f79ffae` | `recover/workerroot-snap-pause` = `0b35b15d` → `0f79ffae` |
| web track (Stage 5) | `v3-web` | `97ba537c` | none — never apply `v3-web-snap-pause` (mutation scratch on an older base; applying it reverts committed code) |

Every other `*-snap*`, `*-wip*`, `*-drafts`, `*-preserve` branch on origin is
superseded history. A snapshot is a `git stash create` commit.

## 3. Host setup (do this first, in order)

0. **OS**: Linux (x86_64 or aarch64). The build-admission scripts and every
   build command use `flock` (util-linux); on macOS install it first
   (`brew install util-linux` and put its `bin` on PATH) and drop the
   linker compression flag (see 4). All gates and the Playwright stack
   were only ever run on Linux.
1. **Hardware floor**: ≥250 GB free disk (each track's cargo target dir grows
   to 10–14 GiB, the workspace gate dir ~11 GiB, plus release builds);
   16+ cores recommended. The old host (8 cores, 111 GB) was the bottleneck.
2. **Toolchains**
   - rustup; the repo's `rust-toolchain.toml` pins `1.98.1` + `rustfmt`,
     `clippy`, target `wasm32-unknown-unknown` — rustup installs it on first
     `cargo` call. Stable only (Dioxus 0.7 breaks on nightly).
   - Bun `1.3.14` (`curl -fsSL https://bun.sh/install | bash -s bun-v1.3.14`).
   - Dioxus CLI `0.7.10`: `cargo install dioxus-cli --version 0.7.10 --locked`.
   - Playwright browsers: in the checkout, `bun install` then
     `bunx playwright install chromium firefox` (Linux may need
     `bunx playwright install-deps`).
3. **Clone and worktrees** (paths below are the convention; adjust the root):
   ```sh
   cd ~/repos
   git clone https://github.com/cefege/roost.git roost-v3
   cd roost-v3 && git checkout v3 && bun install
   git worktree add ../roost-v3-coord  v3-coord
   git worktree add ../roost-v3-worker recover/workerroot
   git worktree add ../roost-v3-web    v3-web
   for d in coord worker web; do (cd ../roost-v3-$d && bun install); done
   (cd ../roost-v3-coord  && git fetch origin v3-coord-snap-pause          && git stash apply 6c6245e4)
   (cd ../roost-v3-worker && git fetch origin recover/workerroot-snap-pause && git stash apply 0b35b15d)
   ```
   Verify: `git -C ../roost-v3-coord status --short | wc -l` = 103 and
   `git -C ../roost-v3-worker status --short | wc -l` = 405 (the worker
   snapshot also restores gitignored `target-track/pause/` drafts and
   `target-track/drafts/`).
4. **Build settings and scripts** (copies live in `docs/v3-handoff/`):
   - `cp docs/v3-handoff/host-cargo-config.toml ~/.cargo/config.toml`. It
     turns off dependency debuginfo and adds linker-compressed debug
     sections for `x86_64-unknown-linux-gnu`; if the new host is another
     triple (e.g. `aarch64-unknown-linux-gnu`, `aarch64-apple-darwin` — macOS
     `ld64` does not take `--compress-debug-sections`, drop that section
     there), fix the `[target.…]` header. Never set `RUSTFLAGS` in a build
     env (it silently replaces the config).
   - `roost-build-slot` (admits N concurrent builds host-wide) and
     `roost-target-sweep` (deletes superseded test binaries): copy to
     `~/repos/`, `chmod +x`, and edit the `dir=` path in `roost-build-slot`.
     Raise the slot count from 2 to what the new host's cores/disk allow
     (rule of thumb: one slot per 6 cores and per 40 GB free).
5. Do **not** install or run any Roost service on the new host yet.
   Production stays on the old host until Stage 4 (see §8).

## 4. Read order (before any code)

1. `CLAUDE.md` (repo operating rules: ≤400-line files, headers, naming,
   logging, design system, failure index).
2. `docs/v3-handoff/README.md` — PAUSE STATE table, per-track status,
   rulings, operating lessons.
3. `docs/v3-handoff/roost-v3-finish-and-cutover-plan.md` — THE plan
   (Stages 0, 2C, 2W, 3, 4, 5, 6; execution model; slice tables). Stage 0
   is complete. The plan wins on conflict with any other brief; v2 source
   wins over the plan's paraphrase of v2.
4. The three track handoffs: `docs/v3-handoff/coord-lead-handoff.md` (on
   `v3-coord`), `worker-lead-handoff.md` "PAUSE STATE" (on
   `recover/workerroot`), `web-lead-handoff.md` "Pause state" (on
   `v3-web`). Read each from its own branch — they are newer there.
   Handoffs sometimes point at `agent://<Lead>.<Slice>` reports or
   `/tmp/...` files: those lived in the old session/host and are GONE.
   Everything that survived is in the branch, its snapshot, and the
   commit bodies.
5. Supporting briefs in `docs/v3-handoff/`: `roost-porting-conventions.md`,
   `silent-no-ops.md`, `roost-web-pump-slice.md`,
   `roost-web-decode-slice.md`, `u1-pump-smoke-map.md`,
   `web-lead-u2-u3-plan.md`, `roost-u2-attach-precondition.md`, and on the
   worker branch `worker-w1-contract.md`, `worker-w1-wiring.md`,
   `worker-v2-map.md`.

## 5. Where each track stands (verified at pause)

**Coord (Stage 2C).** Committed and gated at `5d9d9001`/`b7ad6997`: wave
C-B (worker-link wire, Sync socket + driver, upgrades — live-stack prints
`READY` with the Rust coord), C-BOOT, S4 sessions, C-INPUT, C-SEND, SY3,
C-SCREEN, `bun_abi` restore, shared layout adapter. Gate: 1247/0/0,
clippy 0, lint 0. `AwaitingDomainPort` = **11** committed. Rust coord +
TS worker + TS web: `terminal-delivery` 4/4; `terminal-render` 3/5.
Snapshot `6c6245e4` holds wave 3 **ungated, never compiled together**:
RenderHistory (root cause of the 2 render failures found: `pump_delta`
dropped `terminal_stream_id` from delta meta — fixed, specs not re-run),
CDirect, AtAttach, GsSearch, D1Deploy (pending integrator decisions listed
in the coord handoff), CRetain. In the snapshot the ratchet reads 2
(`SessionsPrompt`, `DiagSnapshot`) because slices flipped rows before
gating — every flip must be re-proved. Waiting merges: worker `5208e0f9`
(agent-status retirement decode, protocol-only; on `origin/v3-worker`) before AG2/C-PUSH; worker
`terminal_capture` protocol types (not yet committed) for C-CAPTURE.

**Worker (Stage 2W).** Wave 1 committed and gated (1223/0/0 ×2, clippy 0,
lint 0): all downstream frames have explicit arms (no catch-all), hello
sends `advertised()`, input/view/stream/pipeline/cells/query owners wired.
Snapshot `0b35b15d` holds waves 2–3 (13 slices: door, heartbeat, capture,
attach, attach-direct, agents ×4, peer, kupdate, resume, durable) wired and
compiling, **ungated**: 3 known reds, most mutations + clippy pending,
WResume's tests never run (`target-track/pause/wresume_run.sh`), capture
protocol types must land as a protocol-only commit for coord. Cross-track
gaps: worker Connect client sends no worker credential; v3 coord does not
fill `recovery_metadata`.

**Web (Stage 5).** Committed: U-0, DECODE (every Firehose arm), PUMP (live
check passed: TS backend + Rust bundle reaches the workbench), wave A (md
primitives + `/design`, themes, UI commands, gamepad/TV, renderer input),
renderer core, wave B (SHELL, SIDEBAR, DECK, TERM mount, SMOKE — 53/53
`SmokeApi` members, 10 reject naming their slice). Gate at `e09f39ca`:
tests 1323/0/4 ×2, clippy 0, lint 0; fmt + wasm32 not re-run. Not done:
both dx bundles, the TERM gate spec, all other U-2 rows. Known gaps: UI tab
close never becomes `SessionsKill`; a 1013 close does not redial at once.

**`v3`** has only the Stage 0 merge (S3.0 green at `78d5dc24`) + docs.
Nothing after Stage 0 is merged into it yet.

## 6. Next steps, in order

1. Host setup (§3). Then on each worktree: `cargo check --all-targets` for
   its crates, and route errors by owning file.
2. Run three **track leads** in parallel (§9 brief rules), each scoped to
   finish inside one budget:
   - **Coord**: gate the wave-3 snapshot slice by slice (re-prove every row
     flip), re-run `terminal-render` with the Rust coord, cherry-pick worker
     `5208e0f9` then AG2 + C-PUSH, X2 DiagSnapshot (needed by
     `__smoke.terminalStreamProbe`), then C-CAPTURE once the worker's
     capture types land. Done = plan's "Track 2C done".
   - **Worker**: gate waves 2–3 (fix the 3 reds, run WResume, mutations,
     clippy), land capture types as a protocol-only commit and send its SHA
     to coord. Done = plan's "Track 2W done" (door serves the SPA; no
     interim arms left).
   - **Web**: finish the gate (lint, fmt, wasm32 ± smoke), add
     `crates/roost-web/dist-smoke/` to `.gitignore`, build both bundles,
     `grep -rc __smoke crates/roost-web/dist` = 0, pass
     `terminal-delivery.spec.ts` "browser smoke flow creates and cleans its
     resources" on dist-smoke; then U-2 rows (SYNC LIFECYCLE, STREAM
     LIFECYCLE, renderer remainder, PAIRING first).
3. Integrator: after each wave, merge tracks into `v3` (trial merge first),
   run the workspace gate (§10), record it in `docs/v3-gate-baselines.md`
   "Phase gates", merge `v3` back into each track.
4. Stage 3 Playwright gates (plan §Stage 3.1–3.2) can run on the new host.
   Stage 3.3+ and Stage 4 must run on the production host (§8).

## 7. Rulings already made (do not relitigate)

- Parity = v2 wins. **One exception:** resumed direct uploads append at
  `bytesWritten`; v2 (`apps/worker/src/attachments/attachment-operation-owner.ts:266,374-381`)
  writes at offset 0 and corrupts the file.
- Worker agent-report endpoint and attachment base live under the v3 worker
  data dir (not v2's `~/.roost/…`), so v3 never touches v2's live files
  during the side-by-side period. Env var names unchanged.
- `KeeperContractV1.bun_abi` restored (v2 requires 1..=128 and compares it
  for restart admission); the Rust keeper reports the constant `"rust"`.
- Capability string is `terminal_metadata_v1` (underscores, v2 spelling).
- Public (unauthenticated) RPCs = v2's 7 exactly, pinned by a test.
- `updateBroker` on POSIX answers "unsupported updater action: <action>" for
  non-START/STATUS, else "Windows update broker command received on a POSIX
  worker" (what a running v2 worker sends).
- 7 downstream kinds have no v2 absent-owner reply (v2 sends nothing);
  interim arms send nothing + warn + test; each must reach its real owner
  before Track 2W is done.
- Route retirement not ported (v2's only subscriber is the unported legacy
  metadata parser). The view-stream controller drop is conditional on every
  `terminal-*` spec passing with the Rust coord.
- One implementation per concept: the layout-document adapter and the
  terminal-view registry live in `roost-protocol`; capture types will too.
- Design lint skips `tests/` (as v2 skips `*.test.ts`); one colour-parser
  line in `crates/roost-web/src/smoke/paint_proof.rs` is baselined (port of v2
  `smokeHarness.ts:231-232`).
- `SmokeApi` has 53 members.
- Windows (update broker, win32 host sampling, Windows CI tier) is PAUSED
  and out of scope.

## 8. Production and the old host

- Production `https://mike.roosttt.com` runs **v2** on the old host
  (`almalinux`, Cloudflare tunnel → Caddy → systemd socket bridge →
  `127.0.0.1:4103`). Stage 3.3 (install gate as a scratch user), 3.4 (rc
  tag + `roost update`) and Stage 4 (import v2 DB, `v3.mike.roosttt.com`,
  flip) are runbooks for **that** host — follow the plan's Stage 4 exactly,
  there.
- On the old host: never kill the v2 keeper (it holds live PTYs), never
  stop v2 services before plan step 4.7/4.8, never touch
  `/home/almalinux/repos/roost` (the user's uncommitted edits; it serves
  `roost-site.service`), `paul-roost-origin-tunnel.service`,
  `~/.cloudflared/config.yml`, or `omp-auth-broker`.
- Release tags: v3 releases are `v3.*` (`.github/workflows/release.yml`);
  none exist yet. First is `v3.0.0-rc.1` after Stage 3.

## 9. How to run leads (lessons paid for on the old host)

- **Never end a turn while waiting** on a build or a helper. A turn that
  ends with no tool call makes the harness force a tool choice the model
  rejects (API 400), which kills that agent and every helper it spawned.
  Wait by blocking in the foreground (`flock <lock> true` with a long
  timeout, or a sleep loop). Never spawn an agent only to wait. Put this
  rule verbatim in every brief.
- **Budgets end in 1.5–3 h.** Scope each lead to what one budget finishes;
  commit + push after every gated item; keep its `*-lead-handoff.md`
  current in every push; near budget: stop spawning, bring helpers to a
  compiling stop, snapshot to a NEW `<branch>-snap-<topic>` ref (verify with
  `git ls-remote`), push, report.
- **≤3 helpers per lead, one level deep.** One build lock per worktree
  serializes a track's builds; more helpers only burn budget waiting.
- Every cargo/dx command:
  `CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=<n> CARGO_TARGET_DIR=<worktree>/target-track`
  and `flock <worktree>/target-track/.roost-build.lock ~/repos/roost-build-slot <cmd>`.
  After every gate: `roost-target-sweep <worktree>/target-track`;
  `cargo clean` between waves when a target passes ~12 GiB; disk floor
  10 GiB free.
- Path-restricted `git add` only (never `-A`); commit subject
  `<area>: <scope>` + a why-body; every new guard seen to fail (mutate the
  guarded line → test fails → revert; record it in the commit body); every
  new variant/flag/value names its production consumer; classify every red
  test against v2 before touching it; `#[ignore]` only as
  `#[ignore = "<slice>: <what>"]`; no stubs, `todo!()`, empty `pub mod`,
  `unwrap`/`expect` outside tests, `macro_rules!`, or top-level mutable
  state; files ≤400 lines including tests (only
  `crates/roost-coord/src/rpc/service_impl.rs` is exempt).
- Shared-file ownership: the integrator owns `crates/roost-protocol`,
  `crates/roost-proto`, root `Cargo.toml`/`Cargo.lock`, `xtask/`,
  `smoke/`, `.github/`, `docs/v3-*.md`. Leads may edit them when needed
  but must land cross-track shared changes as self-contained commits and
  send the SHA to the consuming lead (cherry-pick, not branch merge).
- Snapshot rule for uncommitted work:
  `git add -A && sha=$(git stash create) && git reset -q && [ -n "$sha" ] && git push origin "$sha:refs/heads/<branch>-snap-<topic>"`.

## 10. Gate commands (copy from `.github/workflows/ci.yml`; never retype)

```sh
cargo xtask fmt && git status --short            # must be empty
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast            # two agreeing runs, 0 failed
ROOST_REPO_ROOT=$PWD cargo xtask lint            # 0 violations; report inputs
cargo test --manifest-path third_party/alacritty_terminal/Cargo.toml
bun run test:terminal                            # Playwright oracle (TS by default)
# Rust executables for the oracle:
ROOST_SMOKE_COORD_EXECUTABLE=<release roost> ROOST_SMOKE_WORKER_EXECUTABLE=<release roost> \
ROOST_SMOKE_WEB_DIST=crates/roost-web/dist-smoke bun run test:terminal
bun smoke/terminal/live-stack.ts                 # hands-on stack; prints READY <url> worker=<fp>
```
Release binaries: `cargo build --release -p roost-cli -p roost-keeper`.
Web bundles: `dx build --release -p roost-web --platform web --features smoke`
→ move `crates/roost-web/dist` to `dist-smoke`; then without `--features
smoke` → `crates/roost-web/dist`.

## 11. First message back to the user

After §3 and §4, report: host specs and free disk, toolchain versions,
worktrees + restored snapshots (status line counts), each worktree's
`cargo check` result, and the lead plan you are about to launch.
