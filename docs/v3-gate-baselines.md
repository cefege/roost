# v3 gate baselines

Every phase gate in the Rust rewrite ran the same Playwright suite that
guarded v2. A gate that only says "all tests pass" cannot tell a Rust
regression from a suite that was already red, so each tier recorded what the
**all-TypeScript** stack did on the same machine, and a later gate was read
against that number. That suite lived in the TypeScript tree, which `v3` has
deleted; the Playwright figures below are the v2 reference, and the v3
browser gate is the Rust-stack smoke in the next section.

**The rule this file exists to enforce:** a spec "passes at this gate" only if
it passes in the baseline below. A spec that skips in the baseline must skip
for the same reason; a spec that is newly skipped is a gate failure dressed
up as a non-event.

## v3 gate: the Rust stack end to end in a headless browser, 2026-10-06

`v3` @ `42269185`, Linux x86_64 (desktop-pc), with the fleet's live
`roost3-worker` and `roost-worker` running beside it and left untouched.
Binaries: `target/debug/roost` and `target/debug/roost-keeper`, built from
`cb3f8e0b` (the worker logs `"version":"cb3f8e0b…"`; the two commits between it
and `v3.0.0-rc.11` touch only `roost-web` and the installer, neither of which
these binaries serve). Web bundle: the production `v3.0.0-rc.11` bundle at
`target/fleet/v3.0.0-rc.11/web` (`fleet build` refuses a bundle carrying
`__smoke`), copied into the scratch tree. No harness exists for this:
`roost-bench` boots an isolated v3 stack but pairs by seeding a key, and the
Playwright suite went with the TypeScript tree. Run by hand.

**Isolation.** Every process ran under `env -i` with the table below, so no
inherited `ROOST_*` reached it. That matters on a fleet host: a shell inside a
Roost session carries `ROOST_AGENT_ENDPOINT` and `ROOST_AGENT_SOCKET_PATH`
pointing into the live `~/.local/share/RoostWorkerV3`. `HOME` under the scratch
tree also hides the installed `~/.config/systemd/user/roost3-*.service`, which
`add-machine`, `add-browser` and `roost worker` otherwise read before the
shell. Ports 47213 (coordinator) and 47214 (worker door) were checked free with
`ss -ltnu` first. The keeper socket, pid and capability files, the outbox, the
key and the logs all land in `$SMOKE/worker`.

```bash
# $SMOKE/smoke-env.sh — run any command inside the scratch stack's environment.
SMOKE=/tmp/roost-smoke REPO=$HOME/repos/roost-v3
exec env -i PATH=/usr/local/bin:/usr/bin:/bin TERM=xterm-256color LANG=C.UTF-8 \
  SHELL=/bin/bash USER="$USER" HOME="$SMOKE/home" TMPDIR="$SMOKE/tmp" \
  XDG_DATA_HOME="$SMOKE/home/.local/share" XDG_STATE_HOME="$SMOKE/home/.local/state" \
  XDG_CONFIG_HOME="$SMOKE/home/.config" XDG_CACHE_HOME="$SMOKE/home/.cache" \
  XDG_RUNTIME_DIR="$SMOKE/run" \
  ROOST_COORD_DATA_DIR="$SMOKE/coord" ROOST_COORD_LOG_DIR="$SMOKE/coord/logs" \
  ROOST_COORDINATOR_LOG_DIR="$SMOKE/coord/logs" ROOST_COORDINATOR_BIND=127.0.0.1:47213 \
  ROOST_COORDINATOR_DB="$SMOKE/coord/coord.db" \
  ROOST_COORDINATOR_AUTHORIZED_KEYS="$SMOKE/coord/authorized_keys.roost" \
  ROOST_WEB_DIST_PATH="$SMOKE/web" ROOST_CORS_ALLOWED_ORIGINS=http://127.0.0.1:47214 \
  ROOST_COORDINATOR_URL=http://127.0.0.1:47213 ROOST_WORKER_LABEL=smoke-worker \
  ROOST_WORKER_DATA_DIR="$SMOKE/worker" ROOST_WORKER_LOG_DIR="$SMOKE/worker/logs" \
  ROOST_WORKER_KEY_PATH="$SMOKE/worker/worker.key" ROOST_WORKER_LOCAL_UI_BIND=127.0.0.1:47214 \
  ROOST_KEEPER_EXECUTABLE="$REPO/target/debug/roost-keeper" \
  ROOST_KEEPER_SOCKET="$SMOKE/worker/mux-keeper.sock" \
  ROOST_KEEPER_PID_FILE="$SMOKE/worker/mux-keeper.pid" \
  ROOST_SERVICE_DIR="$SMOKE/service" ROOST_VERSIONS_DIR="$SMOKE/versions" \
  ROOST_SKIP_AGENT_INTEGRATIONS=1 ${SMOKE_EXTRA_ENV:-} "$@"
```

```bash
cd $SMOKE && mkdir -p home tmp run coord/logs worker/logs service versions \
  && chmod 700 run worker && cp -r $REPO/target/fleet/v3.0.0-rc.11/web web \
  && : > coord/authorized_keys.roost
./smoke-env.sh $REPO/target/debug/roost coord &                  # wait for :47213
# add-machine refuses a loopback dial URL, so it is handed an .invalid one;
# only the bearer is kept from the command it prints.
SMOKE_EXTRA_ENV="ROOST_COORDINATOR_URL=https://roost-smoke.invalid" \
  ./smoke-env.sh $REPO/target/debug/roost add-machine --platform linux > add-machine.out
TOKEN=$(grep -oE 'roost_bt_[A-Za-z0-9_-]+' add-machine.out)
SMOKE_EXTRA_ENV="ROOST_BOOTSTRAP_TOKEN=$TOKEN" ./smoke-env.sh \
  $REPO/target/debug/roost worker --coordinator-url http://127.0.0.1:47213 &   # wait for :47214
./smoke-env.sh $REPO/target/debug/roost add-browser > pair-url.txt  # http://127.0.0.1:47213/#pair=…
```

Then one headless Chromium (1280×800): open the pairing URL → click **New** →
**Open terminal here** → click the pane, type `echo roost-smoke-$RANDOM` and
Enter → reload. Teardown: SIGTERM the worker and the coordinator, then the
keeper and its shell (the keeper outlives the worker by design), `rm -rf
$SMOKE`, and re-check `ss -ltnup` and `systemctl --user is-active
roost3-worker`.

| Step | Observed |
|---|---|
| boot | coordinator migrated `0001_init`, served the SPA from disk, listened on 47213; worker redeemed the grant, registered, bound its door on 47214, keeper capability minted under `$SMOKE/worker` |
| pair | the URL landed on the workbench with no prompt; sidebar footer `smoke-worker` with a green dot, status bar `Synced · 0 sessions · 1/1 workers` |
| new terminal | the folder picker opened at the worker's `$HOME` (`/tmp/roost-smoke/home`); **Open terminal here** gave a `mike@desktop-pc:~$` prompt and a `home` row in the sidebar |
| echo | `echo roost-smoke-5089` (a literal) echoed first; then `echo roost-smoke-$RANDOM` printed `roost-smoke-3019` on its own line under the prompt, so the shell, not the input path, produced it |
| reload | both echoes and their output were back in the pane 537 ms after `reload()`; same session URL `/t/<worker-fp>/tmp/roost-smoke/home` |
| carrier | **WebRTC** before and after the reload (`Terminal transport` chip: "Terminal cells and input use a direct WebRTC connection to the worker."); worker: `terminal peer established` `ready_ms` 47, then 28 after the reload |
| teardown | every scratch process exited on SIGTERM; no process matched `roost-smoke`, nothing listened on 47213/47214; `roost3-worker` and `roost-worker` `active`; 4114 still held by the same live worker pid as before the run; the live keeper (on `RoostWorkerV3/mux-keeper.sock`) the same pid mid-run and after |

**Loopback was not used, and why.** The SPA, served from the coordinator
origin, probes the door at the protocol default
`http://127.0.0.1:4114/api/local-bootstrap` (`DEFAULT_WORKER_LOCAL_UI_ORIGIN`),
not at the scratch worker's 47214, so the probe failed (`net::ERR_FAILED`,
twice: first load and reload) and the pane chose WebRTC. On a machine with a
live worker that probe reaches the live door; the browser refuses a door
whose worker fingerprint is not the grant's (`DialFault::ForeignDoor`), so
the probe changes nothing, but a smoke that needs the Loopback carrier must
serve its door on 4114 on a machine with no worker of its own.

**Noise seen, not fixed.** The worker logged one `error` at boot, *"the keeper
force-live retire authorization could NOT be spent"*, because an unmanaged
worker has no service definition to edit; the matching `warn` names the same
missing unit for the spent bootstrap token. The page logged one `WARN
foreground terminal stall … action: resync` (`repair_attempts` 1) a few
seconds after the terminal opened; the pane rendered correctly before and
after it.

## v2 reference: the all-TS Playwright baseline (not the v3 gate)

`bun run test:terminal` on `main` (`96f4db25`), Linux x86_64, 8 cores, run
while the four Rust tracks were compiling. One correctness pass at Playwright's
own worker sizing, then one serial perf pass — the same two passes
`bun run test:terminal` performs, unmodified.

| Pass | Collected | Passed | Skipped | Failed | Wall |
|---|---|---|---|---|---|
| correctness | 145 | 142 | 3 | **0** | 12.3m (4 workers) |
| perf (`@serial`) | 18 | 15 | 3 | **0** | 45.7m (1 worker) |
| total | 163 | 157 | 6 | **0** | 58.0m |

**The baseline is fully green.** The rewrite plan expected
`terminal-peer.spec.ts:61` ("loopback wins before a WebRTC peer is allocated
and keeps Sync metadata live") to fail; it passed in 20.1s. Every later gate
is therefore held to the full 157, with zero tolerance.

### The six skips, and why each is not a Rust gap

Every one is a self-skip inside the spec, keyed to a capability this Linux
box does not have. Each must keep skipping for the same reason; a Rust gate
that makes one of them *run* is a change in platform capability, not a fix.

| Spec | Why it skips here |
|---|---|
| `composer-mobile-input.spec.ts:86` | mobile composer preserves and submits terminal input — mobile viewport capability |
| `terminal-mobile-font-width.spec.ts:260` | WebKit iPhone text inflation — macOS-only WebKit engine, per `playwright.config.ts` |
| `voice-dictation.spec.ts:25` | desktop passive drafting preserves a selected reader — needs the dictation/mic path this box does not expose |
| `perf.spec.ts:282` | trusted key, shallow/deep reveal, child-observed resize |
| `perf.spec.ts:286` | optimistic first marker paints while spawn response is held |
| `terminal-switch-perf.spec.ts:94` | activating a pane that moved while inactive costs one complete baseline |

The last three are perf cases gated on the same precondition as their
sibling cases in those files, which do run.

## Deviation: one spec did not reproduce on `v3`, and the baseline is NOT restated

`bun run test:terminal` on `v3` @ `9b9611f1` (the TS stack, `ROOST_SMOKE_WEB_DIST`
unset) returned **141 passed / 1 failed / 3 skipped** in the correctness pass,
against this baseline's **142 / 0 / 3**.

The failure: `[firefox-peer] › smoke/terminal/terminal-peer.spec.ts:263` —
*"invalid offers, unavailable grants, expired grants, and identity mismatches
fall back without recreating the PTY"*, `@serial`. The stack log line for that
run reads `smoke stack: coordinator=typescript worker=typescript
web=apps/web/dist`, which is the default the knob is required to preserve.

**Reproduced twice, in isolation, and it passed both times**: 1 passed in 45.1s
and 1 passed in 43.0s, same project, same `-g` filter, `--reporter=line`.

**That is not a finding, and the baseline is not being restated.** What the
isolation runs establish is narrow: the spec is not reproducible in isolation.
What they do **not** establish is that it is a flake, and the difference matters
because a claim of "flake" is a claim about cause that nobody has evidence for
yet. The full run put four Playwright workers plus four Rust tracks' cargo on
eight cores — load average 25 to 30 during the run, against roughly 8 for the
isolation runs. A WebRTC offer/grant timeout is exactly the shape that load
breaks and that recovers on its own, so load is a plausible cause and not a
measured one.

**The rule for reading this later.** A later gate is held to the full 157 with
zero tolerance, and this deviation is **unresolved, not forgiven**. The next
full `test:terminal` on this tree must either come back 142/0/3 — which closes
it as non-reproducing under load — or reproduce it, which makes it a real
failure to diagnose. Do not read the two green isolation runs as a cleared
gate; read them as the only evidence that exists, and note what it does not
cover.

## The integrator tree's own gate, after the bootstrap commits

`v3` @ `1c9579e8`, Linux x86_64, 8 cores, run while four track worktrees were
compiling in their own target directories. This is the number the integrator
publishes, and it is the only figure in this file that measures the Rust tree.

| Gate | Result |
|---|---|
| `cargo test --workspace --no-fail-fast` | **1437 passed / 0 failed / 0 ignored** across 173 binaries |
| `cargo clippy --workspace --all-targets -- -D warnings` | **clean**, confirmed on two independent runs |
| `cargo xtask lint` | **0 violations**, `checked 880 inputs`; 35 `xtask` tests pass |
| `cargo xtask fmt` | formatted, working tree unchanged |
| `bun x tsgo -p tsconfig.base.json --noEmit` | **clean** |

**Up from the 1420 the plan recorded at `a464fa3e`**, and the difference is real
work rather than drift: the `AgentStatusOrder` ordering module and its tests, the
`lint_table` rule, and the `xtask` self-tests that came with the DAG and design
changes.

**Two things this number is not.** It does not cover the four track branches —
each is mid-wave, and merging any of them turns it red for reasons that are
progress rather than defects. And it was measured **before** `roost-keeper`'s
lint-table copy lands, which puts that crate under four lint groups it has never
faced. The carry list predicts that merge turns the workspace clippy red, so **a
clean workspace clippy on `v3` is a statement about `v3` and not about the
programme.**

## Where each track stood after it took `v3`, 2026-09-27

Every track branch was merged to `v3` on 2026-09-27, and each was then
measured with `cargo check --workspace --all-targets` in its own worktree. This
is a COMPILE count, not a test count: it is the lower bound a track lead starts
from, recorded so a later track number can be read as progress rather than as
a fresh surprise.

| Track | Tree | `cargo check --workspace --all-targets` |
|---|---|---|
| CLI | `v3-cli` @ `7fbea312` | **0 errors** |
| Worker | `v3-worker` @ `8a85f523` | **0 errors** |
| Web | `v3-web` @ `13ce9833` | **51 errors**, all in `roost-web-terminal` (lib and lib test) |
| Coordinator | `v3-coord` @ `3b043a6b` | not re-measured; its work is already merged into `v3` |

**The web number is a real defect and it was invisible to CI.** `roost-web-terminal`
fails with the SAME 51 errors on `wasm32-unknown-unknown` as on the host, so it
is not a `cfg`-gating gap — it is a half-finished refactor. The renderer was
being split into sibling `impl` files (`cell_renderer/{scrollback,eviction,
history_page}.rs`) and those modules call `CellGridRenderer` methods and types
that do not exist on the parent; `web-sys` features for `Element::{children,
style,class_list}` are missing from the manifest. It went unnoticed because
`ci.yml`'s wasm job builds neither `roost-web` nor `roost-web-terminal`.

**The `kill(-1)` test, run before any `cargo test -p roost-cli`:** commit
`56a6bb59` is an ancestor of every track branch, so
`crates/roost-cli/src/dev/signal.rs` is the fixed version everywhere. The
dangerous version of that file exists on no branch in this family.

## Track gates as they land

### CLI: `v3-cli` @ `b35f0f65`, merged into `v3` as `0ad703be`

`cargo test -p roost-cli --no-fail-fast`, **two agreeing runs**: all 37
`test result:` lines read `ok`, 0 failed anywhere, both times. The baseline on
this branch was **17 failures across 11 binaries**.

**The merge itself is verified, which is a separate claim from the suite.**
`cargo check --workspace --all-targets` on `v3` @ `0ad703be`: **0 errors**,
finished clean. That merge carried 24,231 insertions across 116 files including
a 45-file rustfmt reformat, and a green suite on the branch says nothing about
whether the merge broke a sibling crate. This is the check that says so, and it
is cheap next to a full test build, so it is the one to reach for after any
track merge rather than discovering the answer inside a gate.

**Thirteen warnings came over with it, and they decide the clippy gate on their
own** under `-D warnings`: unused `code` (`api/agent_prompt.rs:102`), `args`
(`api/scrollback.rs:109`), `row` (`api/ui.rs:37`) and a missing `Debug` on
`api/client.rs:42` and `quickstart/install.rs:55`; unused `with`; and unused
imports or bindings in `deploy_release_path.rs`, `deploy_release_stage.rs`,
`update_recovery.rs` and `update_self_replace.rs`. They are pre-existing on
`v3-cli` and owed there, and the fix is to delete the dead binding rather than
to allow the warning away — an allowed warning is a lint rule that has stopped
existing.

The 17 were **6 product defects and 11 wrong expectations**, and the split held
under re-reading. It is worth recording WHY, because "17 red" reads as a broken
port and it was not: the product defects were a payload preview that trimmed
where v2 collapsed in place; a dial-URL resolution that asked
installed-then-ambient *per name* so a shell exporting the most specific name
outranked the installed definition and silently enrolled the next machine at the
wrong door; a service spec that took `GIT_SHA` from the running binary's
compile-time stamp, so every definition named a commit nobody installed; a
failed commit decision exiting 1 instead of 8, which a wrapper reads as
"retry the same transaction"; and — the one that would have broken the cutover —
booleans written with Rust's `Display` (`ROOST_TRUST_PROXY=true`) where the
loader reads `== "1"` and `parse_terminal_peer_enabled` refuses `true`
outright, so installs came up with the wrong front-door policy or would not
boot at all.

That last one had a second defect behind it, found by following the failure one
assertion further rather than stopping at the fix: the Cloudflare Access pair
was written BLANK when unset, and the host refused a blank team domain while
`normalize_https_origin` reads a blank public URL as unset. **Every coordinator
installed without Cloudflare Access got a definition it could not boot from.**
The root cause was in the host loader and is fixed there; the CLI omission is
kept as v2 parity. A test that stops at the assertion it was given will find
the surface, not the cause.

`clippy -D warnings` and `xtask lint` were **not run** on that branch — the
build queue was spent getting the push out. Six pre-existing `roost-cli`
warnings are still owed there, and `xtask lint` has never been run in its
workspace form by anyone.

### Web: the first published gate, and the six values it deliberately leaves

`v3-web` @ `b10646fc`, merged into `v3`. `cargo test -p roost-client-core
-p roost-web-terminal --no-fail-fast`, **two agreeing runs: 47 binaries, 320
passed, 0 failed, 0 ignored.** `wasm32-unknown-unknown` clean, clippy `-D
warnings` 0, `xtask fmt` clean.

**The 51 errors were not a `cfg` gap, and knowing why changed the fix.** The
sibling `impl` files had been written against a *different web-sys than the
lockfile pins*: 0.3.106 has no `Element::style` (it is on `HtmlElement`),
`get_bounding_client_rect` returns `DomRect` and not `Result`,
`Document::create_text_node` returns `Text` and not `Result`, `HtmlCollection`
has no `get`. The fix was therefore a new seam, not a gate — `element_style.rs`
owning the two properties the stable surface does not expose. And inside it, a
choice worth keeping: **`scrollTop` is read and written as a `double` via
`js_sys::Reflect`, because the `i32` accessor rounds a reader that moved half a
row to *unmoved*** — which is exactly the state the follow-band predicate and
the owned-write check are asked about. A rounding accessor would make those two
predicates unanswerable.

**All four web mutation rows bit**, against a gate doc that recorded one
historically did not. Two results inside that: **M-U1 bit wider than
pre-registered** (9 tests, not the predicted one), which means the edit sits
under more behaviour than the row's author knew; and **M-U3's control was
passing for the wrong reason** — green because the defect under test had
inverted its own guard, so it proved the control worked and proved nothing
about the property. Only running the row tells you which of the two you have.

**Six raw values remain in `sidebar.css`, and the baseline now says six.** v2
baselined this same file at **35** and never drove it to zero, so this is
inherited debt and the number is going down (35 → 9 → 6). Three mapped with no
judgement at all; the remaining six are a U-3 design decision and are listed
here so the next person is not rediscovering them:

| Line | Selector | Value | Why it is not a lookup |
|---|---|---|---|
| 432 | `.mobile-deck-count` | `13px` | ramp has 12 and 14, no 13. The sibling `--fraction` block already uses `var(--md-label-m-size)`, so it reads as an off-ramp one-off — but 13 → 12 or 14 changes the badge |
| 604 | `.terminal-card-title` | `13px` | same off-ramp, one step |
| 673 | `.terminal-card-preview-glyph` | `40px` | `--md-display-s-size` is 36px; the 40px in the tree is a `-line` token, not a size |
| 384 | `.roost-zzz-1` | `6px` | decorative floating badge digit, deliberately below the ramp floor of 11px |
| 385 | `.roost-zzz-2` | `8px` | same; 8px exists only as `--md-space-2`, which would be a spacing token doing a type job |
| 659 | `.terminal-card-preview-text` | `7px` | a monospace preview scaled to fit a 28px card |
The last three would need **ramp steps added**, which is designing. A baseline
set at six is the ratchet doing its job — hold the line here, drive it down from
here — and a baseline set at nine without the migration would have been the
opposite. The distinction is not the number, it is whether the number came down
first.

### Coordinator: `v3-coord` @ `81e492b6`

`cargo test -p roost-coord -p roost-host --no-fail-fast`, **two agreeing runs:
728 passed / 0 failed / 0 ignored**, both times. `roost_host --test spa_path`
7/7; `middleware_spa` 7/7; `diagnostics_rpc` 9/9. Clippy and `xtask fmt` were
re-run after the last edits and their results are reported, not predicted.

**The ratchet moved 31 → 27 `AwaitingDomainPort`**, each row flipped in the same
commit whose `service_impl.rs` arm calls real code: `AuthCoordIdentity`,
`MiscMetrics`, `AuditList`, `DiagDebugLogBatch`.

**`mcp_relays_authority::a_publish_the_store_cannot_answer_is_refused_inside_the_busy_timeout` PASSES.** That is the test this whole programme held open from its first measurement, and it is settled by a run rather than by an argument. The cause was a race the reading had missed: the deadline arm already answered `Unavailable`, but the pool's own `acquire_timeout` is the *same* 5 s as `STORE_DEADLINE`, so `sqlx::Error::PoolTimedOut` won the race into the blanket `Internal` mapping. "A connection that was not free is not a statement that failed."

**And the propagation was deliberately refused.** Five other sites map `PoolTimedOut → Internal` — `push/rpc.rs`, `ui_state/fence.rs`, `workers/rpc.rs`, `sessions/tasks.rs`, `rpc_transcription.rs` — and v2 has no pool and reports a busy database as `Internal`. Propagating would have been a parity regression on five methods to make one uniform, i.e. optimising for a shape rather than for a behaviour. **The mapping belongs in one shared helper that takes the domain's declared answer**, so it cannot drift, without silently rewriting five of them.

**Four defects the verification found, and three of them are the reason it was worth running.** One is a live production bug on the front door:

- **`middleware/security.rs` overwrote `Vary`.** The CORS layer runs *outside* the SPA, so it sets the header *last*, and its `insert` dropped `accept-encoding` — the token that says a bundle's two answers differ. A shared cache would have handed a gzipped body to a client that refused gzip. Fixed by merging (`append_vary`) rather than replacing, and the SPA test now asserts BOTH tokens survive so neither layer can regress alone. **The generalisable part: a layer that touches a response header is a candidate for this class whenever the composition order changes, and the order is what makes it possible.**
- **`middleware_spa` used `#[tokio::test]`** whose current-thread runtime starves the task `axum::serve` is spawned onto, and the fixture's `get` is a *blocking* socket read. All seven failed on a 10 s read timeout and **none of them were about the SPA**. Any new listener-backed test binary needs `flavor = "multi_thread"`.
- The export sweep counted only its surplus removals, so a boot sweep emptied the directory and reported zero.
- `roost-host` forbids the literal `v2` anywhere in the crate, and the citations broke its install-identity test.

**Three of the lead's own assertions were wrong about correct code**, and one of those is the finding: it compared two reads of a running clock, which is a coin flip on a loaded machine. **A flaky test is worse than no test** — it spends the reader's trust and returns nothing. Delete it or bound it; do not re-run until green and call that a pass.

`xtask lint` reports 2 violations, both in `crates/roost-keeper/tests/`, and both are `v3`'s rather than this track's: the worker branch has carried the restated keeper lint table and all seven binary-level allows since `84be2a9d`, and they clear when that branch merges. The gate number is 2 pending a queued merge, not 2 with a caveat.

**The permanent backstop, and why the direction it checks is the one that matters.**
`crates/roost-coord/tests/method_route_implementation.rs` checks the method
table against the **IMPL**, where `method_route_coverage.rs` checks it against
the **PROTO**. The proto direction cannot lie silently; the impl direction can: a
row marked `Implemented` whose arm is still `delegated_reply` compiles, passes
every other test in the tree, and tells a reader the method works.

**It found one on its first run, and the shape of the symptom is the argument for
it.** `Sync` was marked `Implemented` with no `fn sync` arm — correct, not a
defect: the retired Connect Sync is answered by a *mounted route* returning 410
before `ConnectRpcService` opens a stream, because a throwing stub would keep the
runtime's abort-listener crash path reachable. So a row and its arm coming apart
produces **not a compile error but a 410 that reads like a routing bug**, which
sends the next person to the router instead of to the table. That is the worst
shape a defect can have, and it is invisible to every other check in the tree.

Four guards make the exception set survive contact: `TRANSPORT_ANSWERED` holds
`Sync` **and a second test asserts that list is exactly the set of `Implemented`
rows with no arm** (a documented exception with no bound on its number is a
ratchet with the pin removed); all 16 `UnwiredInV2` delegations are asserted
**correct**, so nobody "fixes" one into a handler; the implemented count is
asserted **above 50**, so the main test cannot be satisfied by emptying the
column; and the test asserts that it read the file `CoordinatorService` is
actually implemented in — without that it would prove a table true of a file
nobody uses.
### Worker: first-ever total, `v3-worker` @ `8a85f523`

`cargo test -p roost-worker -p roost-keeper -p roost-term --no-fail-fast`, **one
run: 530 passed / 48 failed / 0 ignored, 83 binaries.** No worker total had ever
been recorded before this. **This is a triage baseline and NOT a gate figure** —
one run is not two, and the tree moved substantially afterwards.

**THE FIGURE ABOVE IS A COMPILE, NOT A GATE, and this entry did not say so
until now.** Every "0 errors, 0 warnings" reported for this track on 2026-09-27
was `cargo check -p roost-worker --all-targets`, which compiles and does not run
clippy lints. The track gate is `cargo clippy -p roost-worker -p roost-keeper
--all-targets -- -D warnings = 0`, and **the first measurement of it found 7
errors** across six files and four slices — one too-many-arguments, two
large-`Err`-variant, two unneeded-`Ok`-with-`?`, one useless conversion, one
`clone` on a `Copy` type. All seven predate every commit made that day, and none
would have surfaced under `cargo check`. The branch carried a compile-clean
reading for hours because the wrong command was quoted as the gate — **including
by the integrator, in this file, before it was corrected.**

That is the general form below, and its fifth instance: **a compile is not a
gate, and the number that reads like one is the dangerous one.** A measurement is
only a measurement of what it measured. This paragraph is the correction, not the
replacement — the 48-failure triage stands, because `cargo test` was genuinely
run for it.

The finding that matters is the composition of the 48: they collapse to a handful
of root causes, and **exactly ONE is a product defect**. A reviewer who reads
"48 failing" and concludes "this port is broken" would be wrong.

**The first decomposition published here was an UNDERCOUNT, and the correction is
the useful part.** It said seventeen failures were one fixture constant and nine
were one fixture shape. The real extent is **sixteen call sites** of the non-UUID
cell stream id — not four — across `session_cell_sink.rs`, `session_cell_emit.rs`
and `session_raw_metadata.rs`, so `next_cell_frame` returned `Unbuildable` and
every later assertion saw `Withheld(BaselineOwed)`; and **every `WorkerFp` literal
in the tree is 63 hex characters rather than 64**, across four files including the
17-test `session_support` fixture. Three binaries also hit an unset `HOME`
independently, which is a property of `WorkerBoot::resolve` and not of any one
fixture. The total was right; the explanation of it was not. A published
decomposition that turns out to undercount is worse than none — it sends the
next reader hunting one bug where there were sixteen.

The lesson generalises past this number: **triage a count to its root causes,
then verify the extent of each cause before you publish it.** A decomposition is a
claim about every row in the count, and it is as wrong as a count is if it
undercounts one of them.

The real one is worth its own paragraph. `host/install.rs`'s `closing_quote`
returned the index PAST the closing quote, so `split_once('=')` produced a
variable name with a leading `"` that matched no key, and **every systemd scrub
took the "nothing on this line is the key" branch and reported `removed: false`**.
A redeemed bootstrap token survived in the unit; a spent force-live-retire
authorisation survived to re-authorise killing every PTY on the next restart.
Twenty-nine lines of span arithmetic, and a credential and an authorisation both
failing open.

`roost-keeper` on the same branch: `cargo test -p roost-keeper --no-fail-fast`,
**two agreeing runs, 23 binaries, 0 failed.**


### Two process rules, and both were bought the same way

**A verbal handoff has no receipt.** A slice's third file went uncommitted for
two hours because the author messaged the lead "the dirty file is yours to
stage", reported to the integrator "WorkerLead2 is staging it", and then treated
it as done. Two accurate reports; between them they created a belief, in both
people, that a handover had happened. **No `git status` catches a file both
parties have agreed is someone else's problem.**

The narrow rule that would have caught it: **when you hand a file to another
agent, name it in a message to the integrator as well, and say explicitly whether
it is committed.** Not "X has it" — "X has it, it is NOT yet committed, and I
have re-pushed a snapshot containing it". The author also had a snapshot and a
message; what was missing was one line saying which of their files the snapshot
did and did not contain.

**And the complementary half, which is the one that generalises: read
`git status` yourself and do not trust a list.** The author named two files
because that is what they believed they had authored. A person listing their own
work lists what they are thinking about. The rule that actually caught it was
the integrator running `git status` and comparing, because **that check does not
depend on the sibling having kept accurate track.** Both halves are needed: the
list is a question to ask, never a source to trust.

**A green test does not check where its subject is called from.** An enrollment
call placed *after* the link dial satisfies every test in its own binary, because
the tests exercise the function rather than its position in the boot — and being
after the dial is the defect the slice existed to fix. The same shape as a
`#[repr]`-less enum whose `name()` indexes an array (reorders silently, every log
line stays plausible) and as a doc comment naming an implementation that does not
exist (compiles, reads as settled, implements nothing).

**All three are the same class: something that typechecks and reads plausibly
while being wrong.** The defences are structural, not vigilance — a test that
pins the coupling, a commit body that states where each call sits in the order, a
comment that says what a type actually is. **So: when a reviewer is about to
accept "it compiles", the question is what position the code is in, not whether
it builds.** Read the order before the green.

### A consistency test cannot catch a joint violation

The boot order is a fixed array, and `StepId` is a `#[repr]`-less enum whose
`name()` indexes it by `step as usize`. A new test was written to pin the
coupling, then **mutated by swapping the two middle `BOOT_ORDER` rows with the
enum untouched** — the exact rename-everything failure.

**The new test passed.** The pre-existing test, which hard-codes the expected
name vector, caught it.

The reason is the finding: **a test that only ever asks "do these two artifacts
agree?" is structurally incapable of catching "both artifacts are wrong in the
same way."** After the swap, `StepId::KeeperAdmission`'s arm still points at
index 1, index 1 now says `coordinator-link`, and the enum and the array agree
perfectly. The new test checked a **consistency** property; the mutation
violated a **correctness** property. And **nothing inside the crate knows the
correct boot order** — the enum has no independent idea what the right order is,
so the only oracle is a human-written expectation.

The new tests are still worth keeping, for the two failures a name vector
genuinely cannot see: a step appended to one artifact and not the other (where
`StepId::ALL.len() != BOOT_ORDER.len()` is a panic in `complete` otherwise), and
two rows claiming the same name, which is what a partial swap that copies one
name over the other looks like. **But they are subordinate to the name vector,
and a mutation that changes the name vector is a mutation changing the only
place the correct order exists** — which is a review checkpoint the reorder
wants, not a cost.

**When a mutation does not fail the test you expected it to fail, that is a
finding about the test, not a failed experiment.** This is the second time in one
day a test believed to be guarding a property turned out to guard a different
one — the other was M-U3, whose control passed *because the defect under test
had inverted its own guard*. The rule that follows: **publish which test
actually bit.** The instinct to quietly swap in the test that failed would have
destroyed the information, and the information is the result.

**And the failure message is the deliverable as much as the test is.** The
pre-existing assertion printed `left: ["identity", "coordinator-link", …]`
against `right: ["identity", "keeper-admission", …]` with the architecture named
in the assertion text: the whole drift in one line. A well-formed assertion
carries more than a paragraph explaining it, and the paragraph is the thing to
override.

**One operational rule from the same run: do not mutate a tree a test is
reading.** The `BOOT_ORDER` file was restored from a copy while `cargo test` was
still running against it. The restore was verified byte-identical with `diff -q`,
which made that instance recoverable — but a test reading a file while you write
it produces a result about neither version. Kill the run, mutate, re-run.

### The rig, not the port: three reds in one track that all pointed at the wrong seam

The worker track produced three failures in one session whose cause was the
fixture or the test rather than the code under test. **The product was correct in
all three**, and in one of them the *first half of the same test was already
correct*.

| The test said | The truth |
|---|---|
| 24 retained bytes | the literal `b"before anything was armed"` is 25 bytes |
| "the bootstrap token is not spendable" | `Fixture::start` seeds an issued token with an **empty** binding meaning nobody has spent it, and `redeem` fell through to "spent by somebody else" — a **three-state** thing collapsed into two |
| `build_sha(&MapEnv::new()) == None`, asserted unconditionally | the constant is derived from git **at compile time**, so whether it is `None` is a property of the **tree**, not the test. `left: Some("d7675361…")` reads as a build-identity defect and is not one |

The third is the sharpest, because the `match` two lines above the failing
assertion already branched on that same constant and handled both cases
correctly. **Only the closing assertion ignored it.** `build_identity` preferring
the compiled stamp is correct, and a compiled build having an answer with no
environment supplied is not a leak.

**The cost is not the reds. It is that a reader who works out that the cause is
the rig stops trusting the file** — and the next real defect in it goes unread
for the same reason the last one was misread. So each of these is written up in
the commit that fixed it, and the pattern is here for the next track that meets
it.

**The shape to recognise:** *a failure whose message points at the product, where
the message is produced by something that was never under test.* Ask what the
failure is actually **about** before fixing what it appears to be about. Twice
today the cheap explanation was load and twice the serialised or quiet re-run
killed it; twice the plausible cause was the rig.

**And a related habit, from a lead that suspected a sibling's files and was
wrong:** it wrote "almost certainly WorkerStore's, but I am not asserting that
without the name" — the hedge was correct — and then, after measuring, published
the reversal in the same message as its own fix. **A number stays readable
because people correct it in public.** A lead that quietly drops a suspicion
leaves the next reader unable to tell whether it was ever a suspicion.

### CLI cutover, item 1: logrotate landed, and the gate beside it is NOT green

`v3-cli-cutover` @ `fa61f851`, `2L.2`. A rotation plan and its two systemd
units, one `logrotate.d` entry per role, with the second install a
byte-comparison no-op. Ten new tests, **10/10 green in both full-suite runs.**

**The track gate beside it is not met, and the number is worse-looking than a
green one because it is true:**

- run 1: **379 passed / 0 failed**
- run 2: **378 passed / 1 failed** — `dev_fan_out::a_server_that_cannot_start_names_itself_and_stops_what_already_ran`,
  "coordinator never reported handling the signal it was sent", binary time
  0.44 s
- **The two runs do not agree, so this is not a two-agreeing-runs figure.**
- Two isolated re-runs of that target, 4/4 each. The change under test touches
  no `dev/` file, and the flake is load-dependent and pre-existing.
- `clippy -D warnings` and `xtask fmt` were **not run** on this branch for this
  change.

**And one finding worth carrying to Stage 4, because it is v2's answer and not a
gap: v2 installs nothing on macOS.** `apps/coord/scripts/install.sh:601` and
`apps/worker/scripts/install.sh:518` both branch to
`if $IS_LINUX; then write_unit; write_logrotate; …; else write_plist; bootstrap; fi`.
`RotationPlan::Skipped` names `newsyslog` as the thing that rotates there. A
`roost` that installed a `logrotate.d` fragment on macOS would be *less* faithful
than one that does not.

### CLI cutover gate, at `a13c385d`: 399/0/0 twice, and `import-v2` run for the first time

`v3-cli-cutover` @ `a13c385d`, the tree that supersedes the `fa61f851` entry
above. That entry is left as it was written: it is scoped to that SHA, so "not
green" stays a true statement about it rather than being edited to look better
than events were.

Two full-suite runs, 46 test binaries each:

- run A: **399 passed / 0 failed / 0 ignored**
- run B: **399 passed / 0 failed / 0 ignored**

**These are two independent runs, not one run recorded twice** — worth settling
rather than assuming, because identical totals are exactly what a duplicated log
also looks like. The two files differ at byte 4444, their compile times are 2m35s
and 2m43s, they carry 15 versus 14 distinct per-binary timings, and **test
execution order differs**, which is cargo's parallel scheduling. A copied file
matches byte for byte; a stripped extract has no per-test lines. These have 399
of them, one per test.

`import_v2_copy` **9 passed** and `import_v2_plan` **10 passed** — **19 passed,
0 failed, the first time any of them has ever run.** The row selection, the
different-account refusal and the fingerprint filter all work against a real
fixture database. This is the first evidence any of it does.

**The `dev_fan_out` flake recorded above is neither fixed nor addressed.** The
fix `2L.1c` ported is `the_strict_probe_refuses_a_child_that_never_beat`, whose
subject is a probe that quietly defaults to empty; the flaky test is
`a_server_that_cannot_start_names_itself_and_stops_what_already_ran`, a server
that cannot start. Different concerns. **399/0/0 twice therefore does not prove
the flake is gone**, and a red there on any later run is the known, pre-existing,
load-dependent flake rather than a regression. Re-run that target isolated once
and record both results if it does.

`clippy -D warnings` is **not** claimed on this tree: the splits' rewiring left
12 unused-import errors, and fixing them produced a further 6 of the opposite
kind. The gate closes only on the tree that ships, and it must be re-run on the
merged result — a figure taken on `a13c385d` does not describe `v3` after a
merge.

### The CLI gate re-run on the MERGED tree, at `2cdd0e04`

`v3` @ `2cdd0e04` — the commit that merged `v3-cli-cutover` into the trunk.
The figure above was taken on `a13c385d`, which is **not** this tree, so it does
not describe it. This one does.

| Criterion | Result |
|---|---|
| `cargo test -p roost-cli --no-fail-fast` | **399 passed / 0 failed / 0 ignored, twice**, both exit 0, 46 binaries per run |
| `cargo clippy -p roost-cli --all-targets -- -D warnings` | **0 errors** |
| `cargo fmt -p roost-cli -- --check` | clean |
| `cargo xtask lint` | **0 violations under `crates/roost-cli`** |

**This is the CRATE gate, not the workspace gate.** It says the CLI is green in
company with `roost-host`; it says nothing about `roost-coord` or
`roost-worker`, and the merge changed a shared crate's contract —
`roost-host`'s `pub trait EnvSource` gained a `Sync` supertrait — which a
single-crate gate structurally cannot exercise. The workspace gate
(`cargo test --workspace` twice, clippy, lint, fmt) is S3.0 and **has not run
since the merge.** Do not read this entry as the workspace gate.

**The two runs are independent, and that is worth stating rather than
assuming**, because identical totals are exactly what a duplicated log looks
like. They differ, with compile times 2m08s and 2m17s. A copied file matches byte
for byte; a stripped extract has no per-test lines, and these carry 399 of them.

`import-v2`, now on the merged tree as well: `import_v2_copy` **9 passed**,
`import_v2_plan` **10 passed**. **19 passed, 0 failed**, and still the first time
any of them has run. The row selection, the different-account refusal and the
fingerprint filter have evidence against a real fixture rather than a claim.

**The `dev_fan_out` flake, still not fixed and still not claimed to be.** Four
consecutive greens under load — two at `d5c828f9` and two here — after it failed
twice at `fa61f851`. The honest phrasing is the one the gate's own README uses:
**passed under load at this SHA, not fixed.** Four green runs are evidence about
the machine as much as about the test. If it reappears, re-run that target
isolated once and record both results.

**The runs are committed, and that is not a detail.** All six log files are on
the branch under `gate-evidence/`, added with `git add -f` because the repo
ignores `*.log`. Until then the only evidence for the number this merge rests on
was four files in a worktree, and `git worktree remove` would have taken it. The
README names which run is current and which is superseded, and **keeps** the
superseded entries: `a13c385d`'s tests were green while its clippy was red,
which is the whole reason a green test figure alone never certified this branch.

### Keeper client: the plan's premise was stale

The plan recorded three open keeper-client defects on `v3-worker`
(`wait_for_reply` discarding non-matching frames, six missing client frames,
`resize()` discarding the applied seq and geometry). **All three were already
fixed** by commits `94bab2b7` and `be68afa0`, merged in at `8a85f523`, with 14
tests covering them. The plan was reading a doc line the branch had moved past.
The deferral was proved with a mutation rather than an assertion: reverting
`client_frames.rs:121` to the pre-fix drop gives **0 passed / 2 failed**, both
`left: []`.

The lesson is the one this file exists to enforce. A defect list in a plan is a
hypothesis about a tree; a measurement is a fact about one, and the two drift
apart at exactly the rate the tree moves.


## How to read a later gate

- **Phase 2** (Rust worker, TS coord): no spec that passed in the baseline
  may fail. `terminal-delivery.spec.ts:15` ("browser smoke flow creates and
  cleans its resources") is the load-bearing one and must pass.
  **Its ONLY trigger is the worker track being green** — `UNIMPLEMENTED` at
  zero, all nine `SessionManager` collaborators with production impls, the
  composition root wired. A worker that cannot construct a `SessionManager`
  cannot spawn a PTY, and this gate is exactly "can this worker spawn a PTY".
  **The Rust coordinator is not in this gate at all**: the coordinator is the
  TypeScript one, and `ROOST_SMOKE_COORD_EXECUTABLE` is unset. Reading C-B as
  Phase 2's trigger is wrong, and it was written here first and corrected.
- **Phase 3** (Rust coord, then both): no spec that passed in the baseline may
  fail, with `ROOST_SMOKE_COORD_EXECUTABLE` set alone first, then with both.
  **Its trigger is the WHOLE coordinator track green — C-B *and* C-C, which is
  `AwaitingDomainPort` at 0. Not C-B alone.**
  The terminal specs drive the whole session lifecycle through the
  coordinator's Connect API — workspace create, terminal open, spawn, attach,
  input, scrollback, search, attachment, pane close. **Those are the C-C rows**
  (10 sessions, 11 attachments, 2 search, 1 agent prompt, 2 deploy). Without C-C
  the Rust coordinator answers them `Unimplemented`, so **C-B alone produces a
  working link that carries no method handlers, and the run fails on its first
  spec.** C-B is necessary and not sufficient.
  The "both" run needs all of that plus the worker, and the two halves fail
  differently enough that neither check covers the other: the coordinator's
  fails **loudly at startup** (a 401 on every link, no socket at all), while a
  worker that opens a socket and then cannot serve a session fails **silently
  at runtime**. A green coordinator run would not catch the second, and a green
  Phase 2 would not catch the first.
  **This file has now been wrong about the Phase 2 and Phase 3 triggers twice**
  — first crediting C-B with Phase 2, then crediting C-B alone with Phase 3.
  Both corrections are recorded rather than quietly replaced, because the shape
  of the error is the lesson: **a trigger is a claim about what a gate
  exercises, so it has to be read off the specs the gate runs** and not off
  which wave happens to be finishing.
- **Phase 4** is not Playwright. What is green on this branch is the client
  core's own gate from `docs/phase4-client-contract.md` §1 — the `wasm32`
  build with no `web-sys` in the tree, plus
  `crates/roost-client-core/tests/core_without_a_browser.rs` and the native
  suite. The in-process coord + worker that paints a `MARKER` into a replica
  viewport without a browser was planned and is NOT built here; until it is,
  the marker round-trip is proven by `smoke/terminal/terminal-delivery.spec.ts`
  against a real stack.
- **Phase 5** (all-Rust): the full 157 on Chromium and Firefox, plus a
  production build with the `smoke` feature off containing zero occurrences
  of `__smoke` in the bundle.

A gate that cannot run the suite at all has proved nothing. Say so rather
than reporting a partial pass as a green one.

### CLOSED: `a_deferred_reap_waits_for_the_callers_readiness_barrier`

**GREEN as of `8880699f` on `v3-coord`. The property is closed, and it closed in
R3 — not in R4.** Three records said otherwise until 2026-09-28 and were wrong the
same way: each described a test that was red by design pending a drain. A reader
who dates this fix to R4 will look for it in the wrong commit.

| | |
|---|---|
|**test**|`crates/roost-coord/tests/event_publication.rs::a_deferred_reap_waits_for_the_callers_readiness_barrier`|
|**introduced**|`c02cb9dc` on `v3-coord` — reaches `v3` at the 2C-GATE merge, not before|
|**measured**|`cargo test -p roost-coord --test event_publication` = **6 passed / 0 failed** on `8880699f`. The same binary before R3: **5 passed / 1 failed**. This guard is the one figure measured twice tonight.|
|**green when — TWO halves, and R3 satisfied BOTH**|1. the dispatcher sets `defer_snapshot_reap: true`; 2. a **production reader of the returned `snapshot_reap_ids`** exists. Both hold in `8880699f`. The second assertion for (2) landed in R3, so one green run has meant both since.|
|**why this green means the property, and not a nearby one**|the reader is `frame_dispatch.rs:354` — `self.drain_reaps(&self.handle.worker_fp, &result.snapshot_reap_ids)` — a **dotted read in a file outside the four named producers**. It satisfies conjunct two by **direction**, not by mention. Had (2) stayed a bare grep, this green would have been the v1 failure arriving by a different route: a producer satisfying a consumer's test.|
|**what R4 is, since three records conflated it**|the announced-channel barrier (announce before an `opened`/`respawned` append, commit after the handler settles), the `FrameQueue` producer, and `LiveOrphanKills::attach`/`detach` — so an owed reap has a **socket to travel on**. R3 made an owed reap **drainable**; R4 gives it somewhere to go. R4 is a different property, and its landing does not date this one.|

**The test asks two questions: does anything set `defer_snapshot_reap: true`, and
does anything READ the ids it returns?** Only a caller constructing
`AppendOptions` to defer can answer the first. The declaration
(`events/append.rs:274`), the `Debug` field (`:284`) and the read inside
`build_result` (`:367`) are the only other mentions and none can produce a
`true`.

**IT MUST NOT BE "FIXED" BY EDITING WHAT IT ASKS.** A softened guard is this defect
with a green suite on top, which is how it was found once already: the first version
of this test grepped for a file mentioning the field without declaring it, and
`append_transaction.rs:78` *builds* the field without declaring it — so **a producer
satisfied a test written for a consumer, six of six green, on a capability with no
consumer.** The replacement exists because a name has both a producer and a
consumer and a grep cannot tell them apart.

**Blast radius while it is red, so nobody escalates it as an outage.** The durable
effective snapshot has already omitted the force-closed ids, so **no route is
resurrected** and no browser sees a stale session. What is lost is prompt cleanup: a
force-closed PTY on an offline worker is never killed and becomes one stale session
row per occurrence, until the worker returns or is deleted. **A coordinator that has
never had an offline force-close has never hit this**, which is why no green suite
ever mentioned it.

**If S3.0 reports this failure, the run is correct and the gate is not met. Do
not bisect it, do not soften it, do not skip it. And if S3.0 reports it PASSING,
check WHICH assertions it carries:** a one-assertion green means the flag is set
and the defect may still be live; a two-assertion green means both halves hold.

**An earlier version of this entry claimed the two halves could never be joined
by one green run, and that was wrong.** It is a design choice, not a law: the
second assertion closes it. **What IS true is the general shape this section
collects — a signal that reports success while the thing it reports on has not
happened — and the discipline is to notice it when the test is written rather
than after a reader has been misled by it. Here it was noticed after, which is
the ordinary way these things are found.**

### `target-gate/release/roost` is a STAGE-0 ARTIFACT and must not be gated against

**Found before S3.1 rather than during it, and it is the most dangerous shape in
this file: an artifact that runs, looks correct, and is months out of date.**

Every gate from S3.1 runs against `target-gate/release/roost`. That file exists —
**11 M, alongside `roost-keeper` at 609 K** — and it executes. Measured just now:

```
$ roost --version
dev

$ roost --help          # 12 subcommands
coord doctor help keeper logs reset skill state status test version worker
```

**`update` is not among them**, and the plan's Verification item 1 is literally
*"`roost --help` lists `update`"*. This binary predates the entire CLI track: it has
12 of the 25 subcommands the CLI work delivers, and `v3` has not merged `v3-cli` at
all.

**So a gate run right now would test a build that predates every track.** The
failure is not a crash — it is a suite that boots, serves, and exercises an
11-subcommand coordinator. **A missing binary is loud; a stale one that runs is the
`SocketClose::Default` shape**, and it is the reason this is written down rather than
merely fixed.

**TWO REQUIREMENTS ON S3.0, S3.1, S3.2 and S3.3, neither optional:**

1. **`cargo build --release -p roost-cli -p roost-keeper` on the MERGED tree, after
   the merges** — not the Stage-0 artifact, and not a warm cache that predates them.
2. **Re-run `roost --help` and check `update` is listed** before the smoke suite is
   trusted. The subcommand count is the cheapest available proof the binary is the
   one the gate thinks it is.

**And a second finding, which CORRECTS an earlier claim of mine rather than
adding to it.** `roost --version` prints **`dev`** on this binary, and I read that as
"no local build can satisfy S4.7". **That was wrong, and the two fields are
different things.** Measured in `crates/roost-host/src/build_identity.rs`:

- `artifact_version` (line 13) — `option_env!("ROOST_BUILD_VERSION")`, falling back
  to `dev`. **This is what `--version` prints.**
- `build_sha` (lines 17, 34, 46) — `option_env!("ROOST_BUILD_SHA")`, with a
  resolution chain at line 58 over `DEV_BUILD_STAMP` / `GIT_SHA_ENV` /
  `ROOST_GIT_SHA_ENV`, **so a local build at the tagged commit does carry that
  commit's SHA.**
- `is_compiled` (line 51) — `COMPILED_ROOST_BUILD_SHA.is_some()`. **A field whose
  only job is to say whether the SHA is real or a placeholder.**

**So S4.7's check is a question about `roost status`'s build SHA, not about
`--version`, and a local release build at the tag can satisfy it.** What it cannot
satisfy is a build with no resolvable SHA — and `is_compiled` is how you tell the
two apart. **Read the build SHA and `is_compiled`; do not infer either from
`--version`, which reports a different field and falls back independently.**

(`roost status` on this host prints no build line at all, because nothing v3 is
installed here — the check belongs to S4.3's install, not to a bare status call.)

### `apps/web/dist` is the SMOKE bundle and must never be packaged

**The other half of the stale-artifact family: correct for its purpose, wrong for
every other one.**

S3.1 and S3.2 run with `ROOST_SMOKE_WEB_DIST` unset, which falls back to
`apps/web/dist`. Measured:

```
built 2026-09-27 01:02    0 source files under apps/web/src are newer — it is CURRENT
grep -rl __smoke  ->  apps/web/dist/assets/smoke-By9VXwPV.js
```

**Current AND carrying the smoke backdoor** — exactly right for a Playwright suite
that needs `window.__smoke` to drive the browser, and exactly wrong for anything a
user reaches.

2R already fails the release build if `grep -rl __smoke` finds anything, and builds
`roost-web.tar.gz` with `VITE_ROOST_SMOKE` unset. **That check lives in CI, so it
protects the pipeline and not the tree.** The operational hazard is local: someone
packages the existing `apps/web/dist`, which passes a casual look and ships
`window.__smoke` to production. The installer gates it on
`localStorage.roostSmoke === "1"`, so it is not a remote-execution hole — but it is
shipped code that can drive the app on a user's machine, and Phase 5's own criterion
is `grep -rc __smoke` totalling 0.

**As requirements, not as a worry:**

- **S3.1 and S3.2 may use `apps/web/dist` as-is.** The backdoor is what makes the
  suite driveable; that is its purpose.
- **S3.3's install gate and S4.1's release fetch must not.** S3.3 copies a web
  directory into a scratch install; if that directory is this one, the gate proves
  the installer works *with* a backdoor present, which is a different property from
  the one production has. **Build a smoke-free directory for the install gate, or
  state in the gate record that it ran against the smoke bundle and what that does
  not prove.**
- **Before anything is packaged, `grep -rl __smoke <bundle>` must be empty** — run
  on the directory, not on a build log and not on the pipeline. The pipeline's check
  is real, and it is a check on CI rather than on this tree.

### The questions, not the answers

Everything in this section is an ANSWER. Answers do not let the next person ask the
question, and the question was cheap every single time — it was simply never written
down, so it was never asked by default.

**So here it is. Four questions, in the order they pay:**

1. **Who constructs this?** `grep -rn '<TypeName>' crates/*/src` and read the hits.
   A definition, a comment and an `impl` are not a constructor.
   *Found `runtime/deps.rs` (constructed, never called), `TerminalLiveEffects`
   (inert until R2), `WorkerCapabilities` (three hits, nothing constructs it).*

2. **Does anything READ it, and from which side?** A name has a producer and a
   consumer, and `grep` reads names rather than direction. **A guard whose question
   a producer can satisfy is not a guard** — and the fix is to NAME the producers or
   to match the syntax that only a reader has (`result.field`, not `field:`).

3. **Can this instrument fail?** A check that cannot fail reports success while the
   thing it reports on has not happened. Make it fail on purpose and see whether it
   does. *An underscore binding is how you dismiss a `#[must_use]`, so a `must_use`
   proved with one has proved nothing.*

4. **Is this a NAME that fits where a SHAPE was needed?** A parameter where a field
   belongs; a replace-shaped method where a delta belongs; `Principal::Worker` where
   `append::Caller` belongs; `CellDelivery` where `ChannelDelivery` belongs.
   **The tell is that the two names rhyme and the two shapes do not.**

**And the one that is not a question but a habit:** *when something looks inert, ask
who would CALL it before asking what is missing inside it.* A dependency cycle is far
more often a mis-modelled edge than a real cycle, and re-modelling the edge is what
exposed a kill that would have destroyed live PTYs on the wrong machine.

### GATE ORDERING, decided: `v3-cli` merges FIRST, ahead of 2C-GATE and 2W-GATE

**The dependency, measured.** `v3` itself carries two `roost-cli` size violations
(`command_tree_shape.rs` 432, `update_self_replace.rs` 409, both against a 400 cap
with a 400 baseline). Every track that merged `v3` inherits them, and **they clear
only when `v3-cli` merges.** So **2C-GATE and 2W-GATE cannot read `xtask lint` = 0
until the CLI track's branch is on `v3`** — the two Stage 3 triggers are gated behind
a third track that is neither of them.

**RULING: `v3-cli` merges at 2L.1-GATE, ahead of both.** Three reasons, in order of
weight:

1. **The dependency is real and the alternative is running a gate that cannot pass.**
   2C-GATE and 2W-GATE both require `xtask lint` 0. Running them before the CLI
   merge means running them with a known, named, unfixable-from-there violation.
2. **The CLI gate is the smallest of the three.** 2L.3 and 2L.1c are both bounded,
   and the nineteen `import-v2` tests have never been executed — so its gate is also
   **the first real evidence that the row selection, the account refusal and the
   fingerprint filter work at all.**
3. **It clears two violations from every track simultaneously**, which is the
   cheapest ratchet movement available anywhere in the programme.

**THE COST, stated rather than glossed.** `v3-cli` reaches `v3` before `import-v2`
has been exercised against a real database, which inverts the plan's own instinct to
prove the importer first. **The mitigation is that the merge is the CODE.**
`import-v2` runs as a command at S4.1, after Phase 6, and by then both 2L.1-GATE and
S3.3 have executed it. **So the sequencing risk is confined to the merge, not to the
first real use of the thing being merged** — and the first real use is three gates
away with a gate in front of it.

**What this does NOT change:** the coordinator and worker tracks keep their own gates
and their own ratchets, and neither waits on the other. READY-RING still runs in
parallel in its own worktree. This is a merge order, not a dependency between
tracks' work.

### The gate ratchets, measured so the exit conditions are concrete

Every track gate has a numeric exit condition. **Measured on `v3` at `93a5f69e`
and on both Stage 3 tracks, so the delta is known before any merge rather than
inferred after one:**

|ratchet|`v3`|`v3-coord`|`v3-worker`|gate requires|
|---|---:|---:|---:|---:|
|`PortStatus::AwaitingDomainPort` rows|27|**27**|27|**0** (2C-GATE)|
|`PortStatus::UnwiredInV2` rows|16|16|16|16, keep them|
|`#[ignore = "UNFINISHED"]`|3|3|3|**0** (2C-GATE)|
|`UNIMPLEMENTED` in `roost-worker/src`|**6**|6|**2**|**0** (2W-GATE)|
|`todo!` / `unimplemented!()` in `roost-coord/src`|0|—|—|0|

**`v3-coord` has flipped ZERO rows, and that is correct** — 1b is an impl, not a
row, and a row flips only in the same commit whose `service_impl.rs` arm calls
the real handler. The ratchet is doing its job by not moving.

**`v3-worker` @ `57bd7f74` is at 2 `UNIMPLEMENTED`, down from `v3`'s 6** — both
figures measured on committed trees. On `v3` @ `dbf0edd2` the six are
`runtime/credential.rs:38`, `runtime/link_wire.rs:39`,
`runtime/snapshot_source.rs:39`, `runtime/mod.rs:140` and `:170`, and
`runtime/link_serve.rs:98`. On `v3-worker` @ `57bd7f74` two remain,
`runtime/mod.rs` and `runtime/link_serve.rs` — the latter being the hello
`capabilities` list that 2W-BOOT is briefed to close. `credential.rs`,
`link_wire.rs`, `snapshot_source.rs` and the reconcile block are closed.

**The worker's working tree reads 0 and has NO SHA.** It is four modified and six
untracked files deep into the root construction, and a figure from it belongs to
no commit. I nearly wrote "1" here for the same reason: read a dirty tree, got a
number, and attributed it to a clean SHA two minutes stale.

**A correction worth keeping, because I made it twice.** I read "v3 lacks the
worker's `session/` tree" and concluded there was no figure there to have been 6.
The markers are in `runtime/`, not `session/` — and the very list in that same
sentence named `credential.rs`, `link_wire.rs` and `snapshot_source.rs` as the
closed ones. A true premise, a false conclusion, and two commits made while the
doubt stood. The 6 was measured. **Reproduce a figure at the tree it names, not
at the neighbour of that tree, and not at the directory that happens to be
missing.**

**AND THAT EXPOSED A CONTRADICTION IN THE PLAN, recorded here so the gate is not
failed for someone else's sequencing error.** 2W-DOOR, the local door, was
scheduled AFTER 2W-GATE — while 2W-GATE requires `UNIMPLEMENTED` = 0 and the door
is one of the two markers that must reach 0. **As written the gate could never
pass, because the work that would make it pass sat behind it.** The door is
therefore part of the composition root and comes BEFORE the gate. A ratchet
cannot distinguish "not done" from "mis-sequenced", and neither could the plan.

### `cargo xtask lint` on `v3` is RED right now, and here is what it is

**Measured at `e694506a`, not assumed.** `cargo xtask lint` on this tree prints
**`xtask: checked 2111 inputs` / `xtask: 10 violations`**, and exits non-zero.

Seven of the ten are `roost-keeper` test binaries that compile the shared
`support` fixture without `#![allow(clippy::unwrap_used, clippy::expect_used)]`
at their root — `tests/channel_history.rs`, `keeper_daemon.rs`, `keeper_dispatch.rs`,
`keeper_endpoint.rs`, `keeper_lifecycle.rs`, `keeper_socket.rs`,
`keeper_socket_protocol.rs`. A crate-level allow is a property of the compilation
unit, so every helper site becomes a clippy error the moment clippy runs.

**TRIAL MERGES MEASURED WHAT EACH TRACK'S MERGE ACTUALLY DOES. All ten of `v3`'s
violations are enumerated here, because an earlier version of this entry named
seven and then inferred the other three — which is the exact mistake this file
exists to prevent.**

`v3` alone: **2111 inputs, 0 unreached, 10 violations.**

|violation|file|
|---|---|
|size, 432 lines against a 400 baseline|`crates/roost-cli/tests/command_tree_shape.rs`|
|size, 409 lines against a 400 baseline|`crates/roost-cli/tests/update_self_replace.rs`|
|lint table: exempt, but its COPY restates only `[unsafe_code]`, so `expect_used`, `missing_debug_implementations`, `rust_2018_idioms`, `todo`, `unimplemented` and `unwrap_used` do not apply to the crate at all — *an exemption is a permission, not a substitute for the table*|`crates/roost-keeper/Cargo.toml`|
|fixture `support` compiled without a crate-level `unwrap_used`/`expect_used` allow, 7 binaries|`roost-keeper/tests/{channel_history, keeper_daemon, keeper_dispatch, keeper_endpoint, keeper_lifecycle, keeper_socket, keeper_socket_protocol}.rs`|

Merging each track into a throwaway worktree, guarded sweep on the result. **The
first attempt used `origin/v3-cli` at `b7d09150`, which was five commits stale —
`v3-cli` had unpushed work. It was pushed and the measurement repeated against
the real ref, and the answer did not change:**

|step|head|conflicts|
|---|---|---:|
|`v3` + `origin/v3-cli@3213f798`|`c2939939`|**0**|
|… + `origin/v3-worker@e6a1e8b0`|`3258073e`|**0**|

**`v3` + `v3-cli` + `v3-worker` = `xtask: checked 2425 inputs`, 0 unreached, 0
violations.** Canary-guarded: the sweep fails loudly if `xtask` did not print its
`checked N inputs` line, so this zero is a measurement and not a failed run.

**AND THE FIVE COMMITS ALMOST DID NOT SURVIVE.** `v3-cli` held 5 unpushed commits
— two `roost-host` commits putting the XDG config and state roots behind a public
API, two docs, and a merge — plus 144 uncommitted lines in
`roost-cli/tests/dev_fan_out.rs`, and **the lead whose session owned them is
dead.** Under the plan's snapshot rule the integrator pushed the branch
(`b7d09150..3213f798`) and the uncommitted work to `refs/heads/v3-cli-snap`
(`043a7961`). **Both are on the remote; the working tree was left intact.**

**That near-miss is why the sweep has a canary and the merges are trial runs.**
Work that exists in one worktree owned by a finished session is one `git gc`
away from gone, and nothing in the build or the tests reports it.

**An earlier version of this entry had the attribution backwards twice.** It
said the size violations were "brought in" by the worker merge — they are `v3`'s
own, and the CLI merge is what clears them. And it named `v3-cli` as the fix for
them while that was still an inference, which happened to be correct and was
recorded as though it were known. **Both errors came from reasoning about merges
instead of running them**, and one command in a throwaway worktree answers the
question in under two seconds. The scratch worktrees were removed; neither
measurement touched `v3`.

The new unreached-module rule contributes 0 of these.** Verified by stashing it:
10 violations before, 10 after, with inputs checked going 1480 → 2111.

### Stage 4's one irreversible ordering constraint: sharing a validator, NOT the ordering itself

**The hazard.** `roost import-v2` must run before anything creates the v3
database, because `ensure_self_hosted_tenant` on an EMPTY database creates a fresh
account — and an import arriving afterwards cannot reconcile an account it did not
create. This is the single irreversible ordering step in the plan, and
`docs/FAILURE-INDEX.md` already has an entry for the `config_root`/`XDG_CONFIG_HOME`
divergence, which is the same shape: **a producer and a consumer that must agree,
and can drift.**

**The plan's contingency was to add logic:** *"make quickstart treat an existing DB
whose tenant validates as a rerun rather than a conflict."*

**No logic needs adding, because both sides already ask the same function:**

```
import_v2/mod.rs:223       let tenant = self_hosted_tenant::ensure_self_hosted_tenant(&database, now_ms)
quickstart/grant.rs:164    let tenant = roost_coord::auth::self_hosted_tenant::ensure_self_hosted_tenant(…)
```

`quickstart/grant.rs:29` says it outright: *"`ensure_self_hosted_tenant` is called,
not reimplemented."* And `import_v2/mod.rs:16` names the hazard in its own header.

**WHAT THIS ACTUALLY BUYS, and an earlier version of this entry overclaimed it.**
Sharing the validator covers **one** direction: `quickstart` run AFTER an import
accepts the imported tenant. **The other direction still binds.** If `quickstart` or
a `roost3-coord` boot touches an empty database BEFORE `import-v2`,
`ensure_self_hosted_tenant` creates a fresh account and **the importer then refuses
it as "another install"**.

So the structural answer is worth exactly this: **it converts a silent bad outcome
into a loud one.** A wrong order no longer merges two accounts or half-imports over
a fresh tenant; it stops, with a refusal that names the cause. **That is worth having
and it is not the same as the constraint being gone.**

**`import-v2` first is still a runbook constraint, and it is still binding.** The
earlier claim that "4.1 has nothing to get wrong" was wrong in the only direction
that matters, and is corrected here rather than quietly dropped. What the shared
validator forecloses is a *second* failure on top — a well-meaning fixer adding a
tenant check to quickstart, which is precisely how the two would come to disagree.

### What Phase 6.4 costs: run the classifier, do not read a table

**There is deliberately no count in this section.** It was hand-maintained and corrected
three times — 155 lines, then 70 and 8, then 67 and 12 — because every correction was a
stale number and each cost a turn. **6.4 runs the classifier below against its own tree;
a table written today describes a tree that will not exist on 6.4.** What survives is the
part that does not go stale:

- **The `L11` / `lint-roost.ts` guards go dead with the TS job.** 6.4 deletes the script
  *and* the TS invariants CI job, so every guard citing one loses its enforcement. These
  need re-expressing in `xtask lint` — a different kind of change from a test, and the
  only bucket here that is not mechanical.
- **Two entries have no guard at all**, which the index's own rule forbids: `A defaulted
  injectable host function loses its receiver` and `Roost never owns the agent session`.
  The first explains why — "Bun unit tests pass either way; only the live/Playwright
  browser pass exercises the receiver" — which is a real reason and not an excuse, but a
  reason is not a guard. **Neither is findable by a grep over the tree, so 6.4 would pass
  them by default.** They need a test written, not a path edited.
- **`smoke/` guards are safe.** The plan kept smoke TypeScript as the oracle rather than
  porting it, so those guards keep working untouched. This is the one place the TS
  deletion is a benefit, and it is worth remembering as one.
- **Per-entry counting is required and a line count is not a substitute.** Counting lines
  across the file also matches `**Wrong**`/`**Right**` prose and double-counts entries
  naming two paths. Both happened; the second produced a bucket summing to 105 against 102
  entries.

```bash
python3 - <<'PYEOF'
import re
txt = open('docs/FAILURE-INDEX.md').read()
blocks = re.split(r'^### ', txt, flags=re.M)[1:]
DELETED = ['apps/roost-cli/', 'apps/worker/', 'apps/coord/', 'apps/web/', 'packages/']
b = {k: [] for k in ('repoint', 'lint', 'smoke', 'rust', 'noguard')}
for blk in blocks:
    head = blk.split('\n', 1)[0].strip()
    m = re.search(r'\*\*Guard\*\*\s*(.*?)(?=\n#{2,3} |\Z)', blk, re.S)
    g = ' '.join(m.group(1).split()) if m else ''
    cites_lint = ('lint-roost' in g) or ('L11' in g)
    hits = [p for p in DELETED if re.search(re.escape(p) + r'[A-Za-z0-9_./-]*', g)]
    # a packages/ mention beside a Rust test AND a "was" marker is HISTORY, not a live ref
    hist = ('packages/' in hits and re.search(r'\(?\bwas\b', g)
            and re.search(r'crates/[a-z-]+/tests/[A-Za-z0-9_./-]+', g))
    if hits and not hist:            b['repoint'].append(head)
    elif cites_lint:                 b['lint'].append(head)
    elif 'smoke/' in g:              b['smoke'].append(head)
    elif re.search(r'crates/[a-z-]+/tests/', g): b['rust'].append(head)
    else:                            b['noguard'].append(head)
total = sum(len(v) for v in b.values())
for k, v in b.items():
    print(f'{k:<9} {len(v):>4}')
print(f'{"PARTITION":<9} {total:>4} of {len(blocks)} entries')
assert total == len(blocks), 'BUCKETS DO NOT PARTITION THE INDEX'
print(f'touched by 6.4: {len(b["repoint"]) + len(b["lint"]) + len(b["noguard"])}')
PYEOF
```

The `assert` is the part that matters. It is the check both hand-written versions
lacked, and it is what caught them.

### The import check RECOMPUTES its expected counts, and restates the filter

The Phase 6 install gate asserts row counts after `roost import-v2`. **It must
not assert hardcoded numbers, and the reason is specific.**

Measured once, from `coord_v2.2026-09-26T13-32-32-235.db.gz` (38M gz, 391M
inflated, `integrity_check` `ok`, sha256 `13becff426a0d3f…cb740`): accounts 1,
`account_devices` 26, `authorized_keys` 31 with **26** in the imported set,
`authorized_key_revocations` 241, `app_settings` 7, and one row each in
`organizations`, `organization_memberships`, `account_identities`, `dashboards`,
`dashboard_memberships`.

**THOSE NUMBERS ARE NOT THE GATE, because that file does not last.**
`RoostCoordinatorV2/backups/` keeps a rolling 14 (oldest today `2026-09-11`),
so this one is rotated out around 2026-10-11 and the gate may well run after
that. A hardcoded 26/26/241/7 then fails on a difference that is **correct v2
state** — a device paired or a key revoked since — and the operator spends the
debugging session on the importer instead of on the thing that changed.

**AND `roost import-v2` HAS 19 NAMED TESTS — WHICH IS NOT THE SAME AS 19 PASSING
TESTS, and the difference is the whole point of the entry above.**

**None of the 19 has a recorded pass.** The CLI gate has not run on `v3-cli`, and
2L.1-GATE's two agreeing runs are what would turn this list into a result. Read the
table below as **an inventory of what is specified, not as evidence that any of it
works.** The same distinction cost the worker track seven assertions nobody had ever
executed, on a branch whose clippy, `--all-targets` and lint figures were all clean.

|specified, not yet executed|plan requirement|
|---|---|
|`a_first_run_carries_the_browser_and_leaves_the_machines_behind`|first run, filtered set, machine keys excluded|
|`a_re_run_carries_a_browser_paired_later_and_overwrites_nothing`, `a_re_run_revokes_a_key_the_target_still_holds`|re-run adds a device, applies a revocation|
|`a_target_belonging_to_another_install_is_refused`, `a_target_holding_another_install_is_refused_by_name`|different-account target refused|
|`a_running_coordinator_is_the_only_state_that_refuses_the_import`|live coordinator refused|
|`a_dry_run_writes_nothing_and_reports_what_a_real_run_would_do`|`--dry-run` writes nothing|

**Two of the unspecified-but-listed tests are the ones that matter, and they are
guards on guards.**

`the_filter_is_the_same_predicate_aliased_and_unaliased` asserts the fingerprint
predicate appears in both its aliased and unaliased form — **so editing one copy
and not the other fails the suite.** That is the same discipline as the gate
recomputing the row count in its own SQL: *the importer's filter must not be the
only statement of it.* And `a_dry_run_does_not_migrate_a_target_that_is_already_there`
covers the subtlety that a dry run must not open the target through the
coordinator's own DB open function, since that runs migrations **and a dry run
that migrates is not a dry run.**

**What is NOT covered: the end-to-end run against the real 392 MB backup.** That
is S3.3, and it is the first step in Stage 4 that writes to a database nothing
can undo. The unit layer says the row selection, the refusals and the filter are
right; it does not say the real file opens, reads under WAL, and yields 26/26/7.

**SO THE GATE RECOMPUTES from whichever backup it inflates, by its own SQL, and
prints that file's sha256 so a run is attributable.** The count is not the point;
the *filter* is, and the filter cannot be recomputed by calling the importer:

```sql
-- restated here on purpose: 26 exists ONLY because this predicate runs
select count(distinct k.fingerprint) from authorized_keys k
 where exists (select 1 from account_devices d where d.fingerprint = k.fingerprint);  -- 26
select count(*) from authorized_keys k
 where not exists (select 1 from account_devices d where d.fingerprint = k.fingerprint); -- 5 machine keys
```

**Borrowing the importer's own filter would make the gate assert the filter
against itself** — it would pass with the filter deleted. The two `exists`
clauses above are deliberately written out, and this SQL was run independently
to confirm it reproduces 26 and 5. Everything else is an identity assertion
(imported rows == source rows for that table) and needs no restating.

### Two more, both measured rather than argued

**The worker track's first clippy measurement is 0, at `e6a1e8b0`, and it is 0
again at `14cf518f` (1m 01s).** The second figure was needed because a file added
after the first measurement — `session/journal_sink.rs` — carried a useless
`ReserveError::from(e)` conversion that the first run never saw. **A measurement is
true of the tree it was taken on, and the tree moves**; a published figure is a
statement about a SHA, not about a branch.

**On the first figure.** It is the
**first** — dated, and explicitly not a continuation of a number that was never
measured. The command was `cargo clippy -p roost-worker -p roost-keeper
--all-targets -- -D warnings`, run with `git status --porcelain` empty in the same
shell so the figure belongs to that tree and not the one before it. Seven errors
were found and fixed first; **three of them, and three of the worker's
contribution, would not have surfaced under `cargo check` at all.**

**Three capabilities on that track are unreachable, and the reachability filter is
what found it — not a review and not a test.** `grep -rn 'WorkerCapabilities'
crates/roost-worker/src` returns three hits: a comment, the definition, and the
impl. **Nothing constructs it.** So `SessionManager` cannot be built, the browser
link runs `BrowserLink::detached()`, and three of the nine collaborators have no
production implementation. The same filter found the lead's own `cell_row_json`
nine seconds after a careful read of the diff had missed it.

**That is the argument for making reachability a gate rather than an audit.** A
name-count sweep is not a conclusion — it produces candidates, and a human judges
them. But the one candidate that mattered was invisible to both a diff review and
a passing test suite, and the filter that found it is one `grep`.

**THE RULE THIS PRODUCES, and it is the only one here that closes the class
rather than the instance.** Seven capabilities have now been found unreachable by
one instrument — `adopt_survivor`, `WorkerCapabilities`, `cell_row_json`, the
`LiveEffects` pair, the coordinator's `ClientSeqCursor`, the whole deferred-append
path including its flag, its store field, its claim hand-back and two green tests,
and `WorkerRouteIndex::bind`. In every case the instrument was a `grep` for the type
or the field, and in every case a review, a diff read, and a passing suite had all
missed it.

**THE RULE NOW EXISTS AS CODE, and it found a real file on its first honest run.**
`xtask/src/unreached_module.rs`, wired into `cargo xtask lint`: every `.rs` file
under a crate's `src/` must be reachable from that crate's module roots through
`mod` declarations. The graph is transitive, because a file declared by a module
nothing reaches is itself unreachable and naming the child points at the wrong
file. It is deliberately NOT a dead-code detector — it answers "is this file in
the module graph", not "is anything in it called", and a rule trying to be both
would produce false positives and get deleted.

Swept across all five tracks with `ROOST_REPO_ROOT`: **`v3`, `v3-coord`,
`v3-worker`, `v3-cli` and `v3-cli2` each report 0; `v3-web` reports 8.** Seven
are `find`/`backfill`, queued for registration. The eighth is new and nobody had
named it: **`roost-web/src/platform/peer.rs`, 365 lines of ported WebRTC, declared
by nothing** — and its own header claims *"reached by reflection because the
WebRTC `web-sys` features are not enabled."* That sentence is an eighth instance
of this class, not a defence against it: a comment asserting a reachability the
build does not have, over code no `cargo test` has type-checked.

**NO BASELINE WAS TAKEN, deliberately.** Every one of the eight has a named fix
in flight. The size and console ratchets exist for pre-existing debt nobody is
addressing; baselining work that is queued would make the tree read clean while
eight files stay uncompiled, which is the failure this whole class is about.

**AND THE INSTRUMENT HAS A TRAP THAT PRODUCES THE MOST DANGEROUS NUMBER HERE.**
`xtask lint` shells out to `cargo metadata`. Run the binary without `cargo` on
`PATH` and it exits 1 printing one line, and a `grep -c` over that output returns
**0 — indistinguishable from a clean tree.** That mistake was made twice while
measuring this rule, and it is the same class as every other finding tonight: an
instrument reporting success while doing nothing. A zero from a tool that failed
is worse than an error, because nothing prompts anyone to look again.

**SO THE SWEEP GUARDS ITS OWN OUTPUT — and the canary is STRUCTURAL, not numeric.**

**RUN IT FROM A WORKSPACE.** `ROOST_REPO_ROOT` redirects the *walk* but
`xtask lint` still shells out to `cargo metadata`, **which runs in the process's
working directory**. Run it from outside a Cargo workspace and every tree reports

```
xtask: cargo metadata failed: `cargo metadata` exited with an error:
error: could not find `Cargo.toml` in /home/almalinux/repos/roost or any parent directory
```

which is a **failed run wearing the shape of a result** — the same family as the
`PATH` trap and the one-assertion canary. A helper that prints `TOOL FAILED` without
the message will hide the cause; **print the first line of the output when the
`checked N inputs` line is missing**, because "the tool failed" and "the tool failed
for a reason nobody can guess" are different problems.

```bash
export PATH="$HOME/.cargo/bin:$PATH"
cd /home/almalinux/repos/roost-v3        # or ANY cargo workspace
BIN=/home/almalinux/repos/roost-v3/target-gate/debug/xtask

# CANARY: a FROZEN fixture that contains exactly one orphan by construction.
# Measured, not assumed: it prints "checked 23 inputs" and exactly 1 violation.
canary() {
  OUT=$(ROOST_REPO_ROOT=/home/almalinux/repos/roost-v3/xtask/fixtures/unreached-canary "$BIN" lint 2>&1)
  printf '%s' "$OUT" | grep -q '^xtask: checked' || { echo "CANARY: the tool failed"; return 1; }
  N=$(printf '%s' "$OUT" | grep -c 'not reachable from any crate root')
  [ "$N" = 1 ] && echo "canary OK — the instrument can see" || { echo "CANARY FAILED (got $N) — every number below is fiction"; return 1; }
}

sweep() {
  OUT=$(ROOST_REPO_ROOT="$1" "$BIN" lint 2>&1)
  if ! printf '%s' "$OUT" | grep -q '^xtask: checked'; then
    printf "%-22s TOOL FAILED: %s\n" "$2" "$(printf '%s' "$OUT" | head -1)"; return
  fi
  printf "%-22s unreached %2s   (%s)\n" "$2" \
    "$(printf '%s' "$OUT" | grep -c 'not reachable from any crate root')" \
    "$(printf '%s' "$OUT" | grep '^xtask: checked')"
}
canary || exit 1
for w in roost-v3 roost-v3-coord roost-v3-worker roost-v3-web roost-v3-cli roost-v3-cli2; do
  sweep "/home/almalinux/repos/$w" "$w"
done
```

**The fixture is three files and it is in the tree, not generated.** `lib.rs`
names one module and leaves `orphan.rs` unmentioned; the other rules stay quiet on
a bare crate, so the count is unambiguous. **Run it once when you change the
rule, and again when a sweep looks wrong** — an unexercised canary is the same
defect as an unexercised guard.

**THE FLAW THIS REPLACES, and it is the same shape as everything else in this
file.** The first version of the canary was *"v3-web must print 8"*, taken when
`v3-web` had exactly eight unreached files. **It is now the correct answer for
that tree — the seven `find`/`backfill` files are registered — so the canary
fires on a healthy tree that has simply been fixed.** A canary whose expected
value is a count goes stale precisely when the work succeeds, and a guard that
fires on healthy trees is worse than none: it teaches people to ignore it.

**The property worth canarying is not a number, it is a capability: can this
instrument see anything at all?** A fixture root built to contain exactly one
orphan answers that forever, because nothing anyone does to the real trees
changes it. **Pin a property, not a figure** — which is the same lesson as
`AwaitingDomainPort 27` being a starting measurement rather than an expected
value.

**The verified sweep, with the input count beside every figure so a zero is
never bare:** `v3` 0 of 2111, `v3-coord` 0 of 1607, `v3-worker` 0 of 2414,
`v3-web` **8** of 2363, `v3-cli` 0 of 2124, `v3-cli2` 0 of 2130. Five trees read
zero because five trees were examined.

**THE COUNT WILL KEEP MOVING, so do not cite it — grep, and add to the list.** A
number in this file is a number from the day it was written, and the day this class
grew from six to seven the only thing that changed was that someone ran the filter
again. The list is the durable part; the tally is not.

**AND THE SEVENTH INVERTS THE PATTERN, which is why it is the one worth having.**
The first six are code nobody calls — dead weight. `WorkerRouteIndex::bind` is a call
nobody has made *yet*: it is a method the tree **needed and did not have**, so its
absence was going to force a new public API invented to paper over it. **That is a
stronger argument for the filter than the other six, because it is not only finding
code nobody calls — it is finding the calls nobody has made yet, and the gap between
those two is exactly where a new interface gets invented.** A filter that only found
dead weight would be a cleanup tool; this one finds the seams the next commit needs.

**So: assert reachability, in the tests that already cover the capability.** A
test that proves a value is right is not the same claim as a test that proves
something asks for it. The second is the one that catches this class, it costs one
assertion, and it belongs in the existing test rather than a new file — because the
test is where the camouflage already is.

**And the instance that produced the rule, because it is the first whose guard is
a PASSING TEST rather than a comment — and that inverts the usual expectation.**

**And a fifth instance of the pattern below, which is the first where the guard is
a passing test rather than a comment.** `grep snapshot_reap_ids` outside
`events/append.rs` returns six hits and **not one production reader**: the field
declared, cloned, stored, and dropped. `ClaimOutcome::Claimed` genuinely hands the
stored effect back *including* the ids, and the caller copies them into the
result and returns. So the store is not the missing piece — **the drain after the
claim is.** The `kill_orphan_pty` call site is guarded by
`!options.defer_snapshot_reap`, so on exactly the path where a worker connection
is involved the method is never reached.

The two green tests are the sharpest part: **they assert the ids come back
correctly, and nothing consumes them.** A force-closed PTY on an offline worker is
never killed and becomes a session row that outlives its process, one per offline
force-close. The route is not resurrected — the durable snapshot already omitted it
— so this is prompt cleanup lost, and the shape of the loss is the whole session's
theme: **a green suite is the best camouflage a silent drop can get, because it
converts a defect into an apparent success.**

### An instrument that reports success while doing nothing

**This is the through-line of the whole 2026-09-27 session, and it was found four times before anyone named it.** Each instance looks like it is doing its job, and in three of the four there was a passing gate behind it.

| The instrument | What it reported | What it did |
|---|---|---|
| `middleware/security.rs` overwriting `Vary` | a response with headers | dropped `accept-encoding`, so a shared cache could hand a gzipped body to a client that refused gzip |
| `CoordTerminal`'s `Debug` | two collaborator type names | `type_name::<Self>()` for both fields — it named the container twice, under a comment saying the point was to name which were wired |
| `SocketClose::Default` | a close code | none, and the absence *was* the instruction: a durable append that threw means reconnect and replay, and any code tells the worker to back off instead |
| `LiveEffects` | — | prevented by its own doc, *"no default bodies, because a default that silently does nothing is exactly the history-corrupting drop this subsystem exists to prevent"* |

**The common shape is a component whose output is indistinguishable from its output when the work is absent.** A missing header, a duplicated type name, an absent close code, a defaulted effect: in each case the failure appears somewhere *else* — a cache serves the wrong bytes, a reader concludes a seam is reporting, a worker backs off, a PTY is never killed. Nothing points back.

**The defences, and they are not the same defence in each case.** A merge rather than an `insert`; a name captured at the wiring site so it cannot derive from `Self`; a test that asserts the *absence* is load-bearing; a doc that forbids defaults. Three of those four are structural and one is a comment, and **the comment is the weak one** — which is why the `LiveEffects` methods want tests as well as the doc.

**And the general question, which is the same one this file keeps asking in a different costume: what could this check have detected before you consumed what it returned?** A gate that cannot see the thing prints the same verdict as one that can. That is now five instances here — this one, the unregistered module, the unregistered `WorkerCapabilities`, the `flock` on a missing directory, and the compile quoted as a gate.

### The plan's snapshot rule is incomplete: `git stash create` drops new files

**The rule as written is `git push origin $(git stash create):refs/heads/<branch>-snap`.
It is not sufficient when the uncommitted work includes a NEW file**, because
`git stash create` captures tracked modifications only. A session with 12 modified
files and 1 new module produced a snapshot that silently omitted the new module —
and the omission is invisible, because the ref exists and looks like a snapshot.

**What captures everything, and does not touch the working tree:**

```bash
export GIT_INDEX_FILE=$(mktemp)
git read-tree HEAD && git add -A . && TREE=$(git write-tree) && unset GIT_INDEX_FILE
git push origin "$(git commit-tree "$TREE" -p HEAD -m 'WIP snapshot')":refs/heads/<branch>-snap
```

Verified by counting files in the resulting tree rather than by trusting the ref:
`git ls-tree -r --name-only <ref> -- <dir> | wc -l` read **27 against HEAD's 26**,
and the new module was present. **A snapshot is verified by what its tree contains,
not by the fact that the push succeeded** — the same rule as every other instrument
in this file, and the same failure it is describing.

**And the near-miss was smaller than it looked.** The worker track had already
snapshotted three times on SHA-suffixed refs before this was noticed, so no work
was ever one `git gc` from gone. What was missing was the *current* state, and what
the first attempt of that missed was the one new file.

### `drop()` in an async fn: the compiler hears two different things

**The second instance of the class above, found in the same cascade, and the one
most likely to recur across every async lock in this codebase.**

A `MutexGuard` was released with an explicit `drop()` in one branch rather than by
scope. **`drop()` sets a drop flag, and while that flag is set the borrow checker
still treats the guard as live across every subsequent `await`** — so the future
came out `!Send` and took `SessionManager` with it. Scoping is the only form the
borrow checker can prove.

**The shape: an explicit `drop()` tells the compiler the value is gone, while the
borrow checker's drop-flag analysis says it is still live. The two are answering
different questions, and the second is the one that governs `Send`.** "Drop it
early" is a reasonable instruction and a trap inside an `async fn`.

**And the evening's pattern, now three: the surface is always an ordinary guard
and the real constraint is one layer out.** The deferred-append drain, the
`CoordServices` fields, and now a `MutexGuard` released by `drop()` rather than by
scope. In all three the thing that looked like the problem was a lock or a handle,
and the thing that was actually the problem was a lifetime, a caller, or a bound.

### A mis-modelled edge does not only cost structure — it removes the test that would catch the bug it enables

**Found on the coordinator track, and it is the reason the finding matters more
than the tangle it replaced.**

Four pieces of Wave C-B formed what looked like a hard dependency cycle:
`connection.rs` needed a dispatcher, the dispatcher needed `EventLog`,
`EventLog::new` needed `live_effects`, `live_effects` needed `OrphanPtyKill`, and
the kill's destination was the socket `connection.rs` creates. The available move
was one large commit, and it was defensible.

**The cycle existed because `OrphanPtyKill`'s destination was modelled as *this*
socket. A kill is delivered outbound, on a worker's own writer; the dispatch path
is inbound. Re-modelling the destination as a fingerprint-keyed registry owned by
`CoordServices` dissolved the cycle — and immediately exposed what the wrong
model had been hiding:**

> a process-wide `Option<Outbox>` means a second worker's kill goes to the FIRST
> worker's socket, and a wrong terminal kill destroys a live PTY

**And the test for it, `two_connected_workers_never_receive_each_others_kills`,
could not have been written under the socket model at all.**

**So the cost of a mis-modelled edge is not an inconvenient build order. It is the
loss of the ability to state the property that would catch the bug it enables** —
and that cost is invisible until someone re-models, which may be never. The cycle
was the visible symptom; an untestable cross-worker kill sat underneath it and
would have shipped inside the large commit, with no test able to name it.

**The rule, for the next cycle that appears:** a dependency cycle is far more often
a mis-modelled edge than a genuine cycle. Collapsing the graph into one commit
treats the symptom and leaves the cause in the tree; re-modelling the edge fixes
it. **And the tell that you have the wrong model rather than a hard tangle is that
the thing you cannot express is a test.**

### A green branch is not a tested branch

**Found on the worker track, whose every published number was green.** `clippy
-D warnings` = 0, `cargo check --all-targets` = 0 errors, `xtask lint` = 0 — and
**seven assertions in three test binaries that nobody had ever executed.** They
were red for as long as the branch existed, because the binaries were never run.

The only reason it is known now is that a change made those binaries fail *more*,
which forced the question. Measured by stashing the WIP and running the three
binaries at the earlier commit:

|binary|at `e6a1e8b0`|with the change|
|---|---|---|
|`session_cell_emit`|3 passed / **5 failed**|3 passed / **5 failed**|
|`session_binding`|2 passed / **1 failed**|2 passed / **1 failed**|
|`session_adoption`|6 passed / **1 failed**|5 passed / **2 failed**|

**Seven of the eight are pre-existing and exactly one is new.** So "8 failing"
meant "there were seven and I added one" rather than "I broke eight things" — and
that distinction is only available because the baseline was taken. It went into the
commit body as a before/after table, which is the right place: a regression found
by your own baseline is a better record than one found by someone else later.

**The general rule: a gate that runs a SUBSET of the tests is evidence about that
subset.** `cargo check --all-targets` compiles every target and asserts nothing
about any of them; a clippy figure is a figure about lints. Neither is a figure
about assertions, and the track's other numbers being green is not a reason to
believe any assertion has ever run.

### A grep cannot read direction, so the direction must be NAMED

**This is the closure of the reachability-guard problem, and it took three versions
of one test to get there — which is the honest count.**

**v1 failed open.** It grepped for a file mentioning `snapshot_reap_ids` that did
not *declare* it, and `events/append_transaction.rs:78` **builds** the field without
declaring it. **A producer satisfied a test written for a consumer: six of six
green, on a capability with no consumer.** Not a loose pattern — the wrong question.

**v2 asked a better question** (does anything set `defer_snapshot_reap: true`? —
which only a caller constructing `AppendOptions` to defer can) and went red, which
was correct. But it had **one conjunct where the property has two**, so it would
have gone green one commit before the defect closed.

**v3 names the producers.** The four files that legitimately contain the field are
listed explicitly — `events/append.rs`, `events/append_publication.rs`,
`events/append_transaction.rs`, `events/pending_publications.rs` — and **a file
outside that set must contain it.** The test is one property with two conjuncts, and
its three states are each unambiguous: nothing sets the flag; the flag is set and
nothing outside the producer set reads what it returns; a file outside the set reads
it, which is green and **green means the defect is closed**.

**The rule, and it generalises past this test: an instrument that cannot
distinguish a producer from a consumer cannot guard a consumer.** `grep` reads names
and not direction, so the direction has to be stated — as a named set, which is
checkable, rather than as a pattern somebody tightens until it stops passing.

**The failure direction is the reason this is safe: if a fifth producer is added and
the set is not updated, the test REFUSES a real reader.** That is a bug report. The
v1 failure — a producer quietly satisfying a consumer's test — was silence.

### An underscore binding is how you dismiss a `must_use`, so proving one with one proves nothing

**The fourth instance of "type-checks and does nothing" — and the hardest to
see, because it was in the VERIFICATION rather than in the code.**

A `#[must_use]` guard went on the four `SessionEventSink` methods to stop a dropped
future being built and discarded. **The attribute cannot go on the type alias** —
`EventFuture` is an alias, and rustc ignores `#[must_use]` there with an
`unused_attributes` warning — so the methods carry it. Then the guard had to be
*proven*, and the first proof was:

```rust
let _never_awaited = harness.sink.reserve(…);   // clippy: CLEAN
```

**An underscore-prefixed binding is the documented way to dismiss a `must_use`.**
The violation swallowed the very attribute it was meant to demonstrate, and the run
came back clean. **A proof that passes for the wrong reason is worse than no
proof**, because it gets recorded as a verification.

The working form is a **bare statement** — nothing bound, nothing named:

```
note: a claim that is not awaited is a claim that was not taken
help: use `let _ = ...` to ignore the resulting value
```

**So the rule: the violation must be a construct the dismisser cannot swallow.** A
bare statement, a returned value, a `drop` of it. `let _never_awaited = …` and
`let _ = …` are the same escape hatch, and **a `must_use` proven with either has
proved nothing.**

**Same shape as the coordinator's first reachability guard**, which passed six of
six because a producer satisfied a consumer's test. **Both are a verification that
cannot fail, and both were recorded as checks.**

**And the generalisation: a verification needs its own test.** Nothing about "build
it and see whether the tool complains" is safe when the violation you write is
itself a documented way to silence the tool.

### A construction that type-checks and cannot execute is its own defect class

**Found on the worker track, and the compiler is silent about it by construction.**
A self-referential handle was first filled after the `Arc` was built, with
`Arc::get_mut`. **`Arc::get_mut` requires the WEAK count to be zero**, and the
struct's own placeholder `Weak` is already a weak reference — so the write could
never succeed on any execution. It panicked at runtime on **every**
`SessionManager::new`; thirteen tests died in 0.00 s with *"the Arc was just made
and has no other reference"*.

**`cargo check -p roost-worker --all-targets` returned 0 errors, 53.71 s.** The
library and every test target compiled. **A compile is not evidence that a
program can run**, and this is the sharpest instance yet of the night's recurring
shape: something reporting success while the thing it reports on cannot happen.

**The tell is precise and worth memorising: `Arc::get_mut` on a type that holds a
`Weak` to itself is a write that can never succeed.** The borrow is legal, so the
type checker has nothing to say; only the runtime refcount knows. The construct
for a self-referential handle is `Arc::new_cyclic` — the closure receives the
`Weak`, the handle is correct from the first instant, and there is no window in
which a live manager could report itself unowned.

**And it is the second time a constraint in a brief turned out to be
load-bearing for a reason its author did not know.** The brief said the handle
must be set at construction and not defaulted. The intent was implemented with a
mechanism that defeats it, and only running the tests found it. **A brief that
says "not defaulted" is usually carrying a reason; when the reason is a runtime
one, a reviewer cannot derive it and a test run must.**

### The smoke oracle is runnable — the Stage 3 precondition, verified

Every Stage 3 gate is a `bun run test:terminal` run, and all of them depend on the
TypeScript tree that 6.4 later deletes. **Checked rather than assumed, at
`7b576a59`:**

- `test:terminal` → `bun apps/roost-cli/src/main.ts test terminal` — entrypoint present, 5,924 bytes
- the web bundle — `apps/web/dist/index.html` present (the fallback when `ROOST_SMOKE_WEB_DIST` is unset)
- Playwright browsers — `chromium-1234`, `chromium_headless_shell-1234`, `ffmpeg-1011` installed
- `bun` 1.3.14 on PATH

**So Stage 3 is not blocked on infrastructure.** The one number still missing for
the worker half is the first-ever full `roost-worker` test total, which no commit
in this programme has recorded.

### A count that cannot tell "added" from "moved" is not a count of additions

**The sixth instrument of the night, and the only one that made a false ACCUSATION
rather than a false pass.** A commit reported as *"33 insertions, 15 deletions,
all whitespace and line-wrapping"* was checked with two instruments and declared
to contain new test code:

- `git diff | grep '^\+'` — **prints only added lines, so a reflowed assertion
  looks like a new one.** Wrapping `assert_eq!(SocketClose::QueueOverflow.code(),
  Some(CLOSE_QUEUE_OVERFLOW));` into four lines puts the name on a `+` line
  although nothing was added.
- `git show $c -- <file> | grep -c CLOSE_QUEUE_OVERFLOW` — **`git show` prints
  `-` lines too, so a reflow counts exactly like an addition.** It returned 4.

**The report was true. The commit was pure formatting, and the arithmetic agrees:
three one-line `assert_eq!`s wrapped to four give +12/−3, one `assert!` gives
+4/−1, the `use` block expanding gives +3/−1, and the import line about +2/−0 —
roughly +21/−5, which is the stat exactly.**

**What settles it, and it costs one command — with a correction to this entry's own
claim, found when a 77-file `cargo fmt` pass needed the same instrument:**

```bash
git diff -w --word-diff <a> <b> -- <file>   # a reflow shows the SAME tokens on new lines
git show <rev>:<file> | tr '\n' ' ' | tr -s ' ' | grep -o '<pattern>' | wc -l
```

The second counts ASSERTION STATEMENTS rather than lines mentioning one, so both
commits answer 5 close-code and 3 keepalive — identical, and nothing was added.

**`git diff -w --stat` DOES NOT SETTLE IT, and this entry previously implied it
did.** `-w` ignores whitespace *within* a line, but when a formatter splits one
line into four the diff hunks move and `-w` does not merge them. A whole-tree
`cargo fmt` pass still reports **77 files, 1314 insertions, 576 deletions** under
`-w`, which reads as substantial content change and is not. **`--word-diff` is NOT
the instrument either — it is line-based too, and it shows a reflow as changed
lines. Both were tried on a 77-file formatter pass and both were inconclusive.**

**What settles it is comparing the whitespace-stripped token stream per file:**

```bash
for f in $(git diff --name-only); do
  [ "$(git show HEAD:"$f" | tr -d ' \t\n')" = "$(tr -d ' \t\n' < "$f")" ] || echo "CONTENT: $f"
done
```

**And normalise trailing commas as well**, because a formatter adds and removes
them when it re-wraps: on the web track's pass, whitespace-stripping alone flagged
**56 files** and 56 still differed after `s/,)//g`, so the flag is only meaningful
once both are applied — and a file it still names is a **real** content change, not
a reflow.

**The lesson is the one this whole section is about: I read one file's
`--word-diff`, saw tokens on new lines, and called all 77 files pure formatting.
A sample read as a conclusion is the same error as the accusation this entry
corrects** — and it is the reason the instrument has to be mechanical rather than a
glance.

**And the general rule the pair of instances produced: a tree-wide formatter pass
is its own commit, every time.** The coordinator's `cargo fmt` touched five test
files it had not authored; the web track's touched 77 across three crates, mixed in
with the registration work those same files needed. **A reviewer reading "the
formatter's pass" skips the diff, so a commit whose subject says "formatting" must
contain nothing else.** The coordinator's commit was exactly that and needed no
repair: its ten test functions and 43 assertions are identical across `7c78b57b` and
`0d4df396`, and the close-code assertions in it were **rewrapped, not added**. A
commit's subject line is the only thing that tells a reader which changes were
intended, and a formatter's are not.

**The general rule, and it is the same as every other entry here: an instrument
has to be able to fail before its number means anything.** `--stat` counts lines,
`grep` counts mentions, and both read a moved line as a new one. **A measurement
that cannot distinguish two cases cannot support a conclusion drawn across them**,
and the tell is that two instruments agreed — which felt like confirmation and was
in fact the same blindness twice, pointing the same way because they were the
same kind of instrument.

### A green check on a file nothing includes is not a check

A draft put a struct field inside an `impl` block. Rust reports that as an
error, **but only if it looks at the file** — and it did not, because the
module had not been registered in `mod.rs`. So a `cargo check` run against the
tree came back **clean** while the file being written could not possibly
compile. The lead deleted the file rather than push it, and the finding is
recorded because the shape recurs: **a check that returns zero because the
compiler was never pointed at the thing you changed is not evidence about
anything.**

The rule: **register the module in the same edit that writes it.** If a file has
to be written in stages, the stage boundary is where the `mod` line goes — not at
the end, and not in a follow-up. A sibling crate in this programme has the same
exposure in a different form: a test binary that compiles a shared fixture
without declaring its allow *at its root* was invisible to clippy for the same
reason, and the lint only saw it once the binary was on the list.


**The second instance was found by probing rather than by reading, and the first
draft of this entry had both of its facts backwards.** A build directory was
removed out from under a track, and the command was
`flock <dir>/.roost-build.lock cargo check … 2>&1 | grep -E '^error'`. It came
back in **0.22 s with no output** on a crate whose check takes 50 seconds, and
was read as clean. What actually happened, established by probing `flock`
directly:

1. **`flock` CREATES the lock file it is given.** With the lock file moved away,
   `flock <dir>/.roost-build.lock true` exits 0 and recreates it. **So the
   existence of the lock file proves nothing** — it is created on first use, not
   found. A guard that tests whether the lock is present is testing something
   `flock` manufactures.
2. **A missing parent DIRECTORY makes `flock` fail**, with exit 66 and
   `cannot open lock file`. So the guard this file would naturally carry —
   *"if `flock` fails, stop"* — **did** have something to fire on, and `flock`
   **did** fail correctly. It was not defeated by `flock`; it was defeated by
   nobody reading the exit status.
3. **The pipeline then hid the failure twice over.** `grep -E '^error'` matched
   nothing, so it exited 1 — but what was read was the *output*, which was empty,
   and empty was read as clean. And the structural hazard runs the other way
   too: without `pipefail` a pipeline's status is the **last** command's, so a
   cargo failure that `grep` *does* match on becomes a pipeline exit of 0. A
   green exit from `| grep` can mean the build failed and was filtered into
   nothing.

So the rules, corrected: **a lock that is CREATED rather than FOUND proves
nothing about what is behind it**, so assert the directory; **`set -o pipefail`
so a real failure is not masked by a filter that swallowed it**; and **check the
exit status, not only the output**, because the most dangerous thing a failed
build produces is no output at all.

```sh
set -o pipefail
export CARGO_TARGET_DIR=<worktree>/target-track
test -d "$CARGO_TARGET_DIR" || { echo "MISSING TARGET DIR" >&2; exit 1; }
flock "$CARGO_TARGET_DIR/.roost-build.lock" cargo "$@"
```

`test -d "$CARGO_TARGET_DIR"` and not `test -d <path>`: the hazard is specifically
that the variable names a path `cargo` will silently ignore, so the thing to
assert is the **variable's value**, which cannot pass on a typo between the
brief's path and the worktree's.

**The four silences, which are one class and not four small mistakes.** A filter
that matches nothing; a package that no longer exists; a `--test` that names no
target (which prints a *suggestion*, and reads past easily); and a missing target
directory. Each was met separately and filed as its own incident. **Silence is
the only output all four share**, and grouping them is what makes `test -d` a
rule rather than a patch for one evening.

### A commented-out `pub mod` is a silent un-registration, and the rule for it is NOT a marker list

A subagent disabled three module registrations in a row with
`// TEMP-DISABLED pub mod predictive_echo;`, then `// TEMP pub mod
attachments;`. Each one **removes a module without failing where the edit was
made**: the files stay, the crate still compiles, and the failure surfaces in a
different crate as `could not find predictive_echo in client` — four files from
the cause, phrased as a missing module rather than a commented-out line. The
reader goes looking for a file that is present. It is the same class as a green
check that never ran, and it cost a lead three restores and a build cycle.

**The marker names are unbounded, so a lint listing them is not the fix.** A rule
naming `TEMP`, `FIXME`, `XXX` and `for now` passes on the next spelling. And the
obvious shape test — a `mod` or `use` with a `;` after it — **flags real prose in
this repository**: `// happens to declare \`mod api_support;\`.` quotes a
declaration and disables nothing, and `// this use of the term is deliberate` has
a `use` in it. A lint that cries wolf on the first file it reads is worse than
no lint, because the next occurrence is ignored.

**So the rule was written, made its own tests pass, flagged prose, and was
deleted.** A gate that is broken is worse than a gate that is absent, and an
optional rule is not worth a broken `xtask`. What survives is the rule written
here rather than in code:

- **A module is registered, or it is not in the tree. There is no third state,
  and no marker that temporarily un-registers one.** An unregistered file is
  **never compiled** — it is unchecked text, which is exactly the "green check on
  a file nothing includes" case in the section above. Every type drift in it
  stays invisible until somebody writes the code that reaches it.
- So a module written and not yet reachable is **registered anyway, and reported
  as "declared, no adapter yet."** A trait with no implementation compiles fine
  once registered — at worst a `dead_code` warning, which is allowable per item
  with a reason. Unregistered, it is not checked against the crate's types at
  all, which is strictly worse than a warning.
- If a slice needs a registration it does not own, it says so in its report and
  the integrator writes the line. A slice that cannot compile without a `mod` it
  does not own is reporting a dependency, not working around one.
- Whoever eventually writes the lint should match a bare `mod`/`use` token within
  the first few words of a `//` comment **and** a `;` on that line, and should
  carry the two prose lines above as named regression tests, because a version
  that flagged them would be reverted by the first person who met it.

**The backstop, scoped so it does not become a superstition: a check faster than
the crate has ever checked has not checked.** It bites on `cargo check` and
`cargo test`, and explicitly **not** on a source-tree scan — `cargo xtask lint`
walks files and counts lines, so 0.62 s for it is genuinely fast and does mean
what it says. A rule that fires on every fast result trains people to ignore it.

**A gate that reports nothing is not a gate that passed.**

**The general form, and it is the fourth instance today:** an instrument that
cannot see the thing reports success. A sweep that counts only its surplus
removals reports zero for a sweep that emptied the directory. A rate window that
matches nothing is zero violations. A `SpawnNotAcknowledged` under load reads as
a refused spawn. **Ask what the check could have detected before consuming what
it returned** — that question has caught more real defects today than any
individual test.

### A SQLite digest is only comparable after every connection is closed

**The rule: anything that compares a database file byte for byte must close
every connection to it first, and the comparison must say when it took its
snapshot.**

SQLite checkpoints its WAL and writes the main file when the **last connection
to a WAL database closes** — not when a write happens. So a database can be
byte-identical at the moment a function returns and different a moment later,
with no writer in between. An assertion that hashes the file inside the function
that used it is a race with a timer on it, and it fails intermittently in a way
that reads as corruption.

This is not only a test artefact and it was not found by a test. **`roost
import-v2 --dry-run` has to answer "what would change" about a target
coordinator database, and Stage 3.3's install gate and Stage 4's cutover both
compare digests across a live coordinator.** A false difference there reads as
"the import corrupted the database", which is the one conclusion an operator
cannot afford to draw and cannot easily disprove. So:

- Close the pool explicitly before any caller reads the file, and say so in the
  assertion's own text — an assertion that does not say when it snapshots is the
  defect.
- **And a digest is not the right instrument for a live database anyway.** A
  count of the rows that matter is what proves the import did its job; the digest
  is what proves nothing moved, and for that the file must be closed first.

The related discovery, from the same work: `roost_coord::db::open` **runs the
migrations**, so a command that promises not to touch a database must not reach
it through that function even to look. `--dry-run` asking a read-only handle
whether the target has an `accounts` table, and reporting a first run when it
does not, is the shape that keeps the promise.

### Two more, both of which cost a whole agent-hour to learn

**A diagnosis that survives only the fix it proposed is not a diagnosis.** A
worker test failed on a 5 s `SpawnNotAcknowledged`, which reads exactly like the
load story above, so the explanation was load. It died on a **serialised** re-run
— same result, no other track building — and what was underneath was
`KeeperFixture::start`: a strict accept/serve loop that serves exactly ONE
connection at a time. A test holding the first pool alive across a second
connect makes that second `connect` retry to the timeout, and the failure it
produces **names a SPAWN rather than a connect**. So the rig lied about which
seam it broke, and the plausible cause pointed at the product.

The rule is not "be more careful". It is: **if your explanation is load, and the
re-run under quiet conditions reproduces it, your explanation was wrong.** Load
is the cheapest available explanation and it is the one most likely to be
assumed rather than eliminated.

**A test red for a reason unrelated to what it tests trains a reader to ignore a
red in that file.** Two of this track's reds were literal-versus-assertion, not
logic: one asserted 24 for a 25-byte literal, another named a spawn when the
rig had broken a connect. Neither is expensive to fix, and both are expensive to
leave, because the cost is not the wrong assertion — it is that the file stops
being read. **When a test fails, check what the failure is actually about before
fixing what it appears to be about.**

**And the one that is a process rule rather than a testing rule: a shared
working directory is not yours.** `git add -A` across a tree three agents were
editing captured a sibling's uncommitted fix in its pre-fix state and silently
reverted their work; neither noticed until a test failed, and the fix existed in
exactly one place for as long as that took. Stage an **explicit path list** and
ping the owner before committing anything you did not write. `-A` cannot
distinguish "I read this and it is right" from "this was on disk", and on a
shared tree the second is most of it. An unstaged file is a five-second fix; a
wrong commit is a red tree for everyone.


### Two ways to misread a red run before you have read it

**A `SpawnNotAcknowledged { timeout: 5s }` under load is LOAD, not a refused
spawn.** `SPAWN_ACK_TIMEOUT` is 5 s and it is not a fixture's to widen. A test
binary that opens REAL PTYs competes with every other thing compiling on the
machine, and the first observed instance came from a real keeper starved by
three concurrent track builds. At a gate, that failure is evidence about the
machine. Reading it as a product-seam defect sends you to debug a keeper that
was never asked a question it could not answer, and the expensive part is the
hours, not the mistake. **If a spawn timeout is the only red, re-run it with
the machine otherwise idle before believing it.**

This is the same shape as the known `terminal-peer.spec.ts:263` deviation: a
spec that passes isolated and fails under full load is a load signal until a
quiet full run says otherwise.

**A compile error in one crate is a COMPILE error, not a verdict on the port.**
The worker track's first recorded total was 48 failures across 83 binaries, and
it would have been easy to read that as a broken port. Twenty-nine lines of
span arithmetic in a systemd scrub were the only product defect in it; the
other 47 were seven fixture-shape problems — a `WorkerFp` that wants 64 hex
where the fixture passed a UUID, a cell stream id that must be a UUID, an unset
`HOME`. **Triage a red count to its root causes before you characterise it.**
A count and a character are different claims, and only one of them is
supported by the number.

**A test that has never RUN is a liability, and a red one is information.** Five
named tests in the worker track were written and never executed, blocked behind
a `String`/`&str` in a fixture. Reporting them as "the slice is written" would
have been a claim about bytes rather than about behaviour, and the gate would
have inherited four of them as unverified. When a figure is unrun, say
`unrun` — an unrun gate item labelled unrun costs nothing, and a claimed one
costs the gate.


## Phase gates

**The section the plan requires, created before the first result rather than
under time pressure.** Every entry carries five things and nothing else, because
a gate record that omits one of them cannot be compared with the next one:

|field|why it is required|
|---|---|
|**tree SHA**|a figure without one belongs to no tree and cannot be re-checked|
|**date**|the same command on the same SHA can differ across days|
|**the exact command**|two runs of "the tests" are not comparable|
|**pass / fail / skip, per run**|the TS baseline's skips are named, so ours must be too|
|**agreement across runs**|one run is an observation; two agreeing runs is a gate|

**THE TS ORACLE, which every figure below is read against:**

|pass|expected|
|---|---|
|correctness (serial)|**142 passed / 0 failed / 3 skipped**|
|perf (`@serial`)|**15 passed / 0 failed / 3 skipped**|

**The six named skips must keep skipping for the same reason.** A skip that
starts skipping for a NEW reason is a regression wearing a skip's clothes, and
it is the one failure mode a pass/fail/skip summary cannot show.

### Gate results

**S3.0 WORKSPACE GATE — GREEN at `78d5dc24`, 2026-09-28.** Tree: `v3` after
merging `v3-worker@cceaf15a`, `v3-coord@f3a717a6`, `v3-web@10822254` (zero
conflicts; checked first with `git merge-tree --write-tree`) plus `78d5dc24`,
which fixed the two guards the merged tree failed (`v3_install_identity` and
`join_script`, both inherited from `v3`, both seen red first). Target
`target-gate`, `CARGO_BUILD_JOBS=8`, host `~/.cargo/config.toml` debug
settings (dev/test line tables, no dependency debuginfo, compressed sections).

|criterion|result|
|---|---|
|`cargo xtask fmt`|exit 0; `git status --short` empty|
|`ROOST_REPO_ROOT=$PWD cargo xtask lint`|**0 violations, 2863 inputs**|
|`cargo clippy --workspace --all-targets -- -D warnings`|exit 0|
|`cargo test --workspace --no-fail-fast` run 1|395 binaries, **2950 passed / 0 failed / 15 ignored**|
|`cargo test --workspace --no-fail-fast` run 2|395 binaries, **2950 passed / 0 failed / 15 ignored**|

The 15 ignored: the three `#[ignore = "U-CARRIER: …"]`/`"U-ATTACH: …"` tests
in `roost-client-core` plus 12 `ignore`-fenced doc-test blocks in `roost_proto`; `grep -rn
'#\[ignore' crates` finds exactly three attributes, all in the named form.
`roost-coord` has none. The NOT GREEN record below is kept as history.

**S3.0 WORKSPACE GATE — NOT GREEN. Two of four criteria met. 2026-09-28.**
First run of this gate on a merged tree; it had never been run since the CLI
merge, and the gate script had refused four times for want of a quiet machine.

**Count all four, because counting only the ones that were run is how this gets
overstated.** A gate is two agreeing green runs of the track's crates, clippy 0,
`cargo xtask lint` **0**, and `cargo xtask fmt` clean. Here:

- **fmt — MET.** Exit 0, re-verified at `01370d2f` with zero diff lines.
- **lint — UNMET.** **8**, every one `roost-keeper`: its lint table plus 7
  fixture allows. The ladder below says in advance that the `v3-worker` merge
  takes them to **0** — a known outstanding rather than a new finding, and still
  outstanding.
- **clippy — MET.** `cargo clippy --workspace --all-targets -- -D warnings` on
  `8c332da8`: **exit 0, zero errors**, every crate and every target. First run
  of this command on a merged tree.

  **Be exact about what that does and does not prove, because this file has
  already been wrong about it once.** Clippy is **not** what finds an `!Send`
  future held across an await, or a trait whose signature no implementation can
  satisfy. Those are **rustc** errors, and they surface only once something
  actually spawns the future or implements the trait. R3 is the proof: it found
  both — `AppendOptions` was `!Sync` so the append future was `!Send`, and
  `handle_durable` returned `Pin<DispatchFuture>` where `DispatchFuture` is
  already `Pin<Box<dyn Future + Send>>` — and it reported clippy **0** in the
  library and in every target it added. **Clippy 0 means the code is clean under
  clippy. It does not mean the impossible things are absent.** Only a first
  implementation finds those, and that is why the "two agreeing green runs"
  criterion below is the one that has to stay open.
- **two agreeing green runs — NOT RUN.** No `cargo test` has been executed here
  at all.

**Two met, two outstanding.** `roost-cli` alone had already been checked under
all three of clippy, fmt and lint before the workspace run, because the CLI
track reported its gate half as unrun and that code was already on this branch.
It is: `cargo clippy -p roost-cli --all-targets -- -D warnings` exit 0, `cargo
xtask fmt` exit 0, and `cargo xtask lint` with **0 in `roost-cli`**.


**A workspace that compiles and a binary that answers `--help` are not a gate.**
The first revision of this block said "one criterion is unmet" — counting only
the criterion that had been run, and reporting a number where the honest answer
was a fraction of the whole. The second said "one met, three unmet or unrun",
which was right at the time and went stale the moment clippy was run. **A status
line is a measurement too, and it rots the same way a number does.**


**Each row names its own tree.** They are not the same commit, and hanging one
SHA over the whole block is the mistake this file exists to prevent.

|check|command|tree|result|
|---|---|---|---|
|workspace|`cargo check -p roost-coord -p roost-worker -p roost-host --all-targets --keep-going`|`8608fd44`|**exit 0, 0 errors**|
|release|`cargo build --release -p roost-cli -p roost-keeper`|`8608fd44`|**exit 0**; `roost` = 13,907,072 bytes|
|`update` wired|`roost --help` on that binary|`8608fd44`|`update — Replace this binary with the latest published v3 release`. **Present.** 23 subcommands|
|fmt — failed, then fixed|`cargo xtask fmt`, then `cargo fmt -p roost-host`|`8608fd44` → `7ea8324c`|**exit 1** (3 `roost-host` files) → **exit 0**|
|fmt re-verified|`cargo xtask fmt`|`01370d2f`|**exit 0, 0 diff lines**|
|host recheck after the fix|`cargo check -p roost-host --all-targets`|`7ea8324c`|**exit 0**|
|lint|`cargo xtask lint`|`01370d2f`|**8, every one `roost-keeper`** (1 lint table, 7 fixture allows). **0 in `roost-coord`**|
|clippy, cli only|`cargo clippy -p roost-cli --all-targets -- -D warnings`|`8c332da8`|**exit 0, 0 errors**|
|clippy, workspace|`cargo clippy --workspace --all-targets -- -D warnings`|`8c332da8`|**exit 0, 0 errors** — every crate, every target|
|two agreeing green runs|`cargo test --workspace --no-fail-fast`|**NOT RUN**|no `cargo test` has been executed on this tree. The 1734/1/15 figure elsewhere in this file is from `3e92e97e`, which is the v3-coord merge and **predates the CLI merge `dbf0edd2`** — it does not describe this tree.|

**The risk this gate existed to close is closed by compilation, not by a grep.**
The CLI merge changed `pub trait EnvSource` to `pub trait EnvSource: Sync` in
`crates/roost-host/src/env.rs` — a crate three others consume. The CLI gate ran
`-p roost-cli` only, so it proved the four `EnvSource` *impls* satisfy the bound
and said nothing whatever about `roost-coord`, `roost-worker`, or their consumer
sites. Re-measured on `01370d2f` by
`git grep -c EnvSource -- crates/roost-coord/src crates/roost-worker/src`:
**11**, in three files — `roost-coord/src/serve.rs:65` (1),
`roost-worker/src/browser_commands/file_commands.rs` (4: the `use` at `:27`, and
`Arc<dyn EnvSource + Send + Sync>` at `:93`, `:107`, `:127`), and
`roost-worker/src/runtime/boot.rs` (6: the `use` at `:19`, then five
`&dyn EnvSource` parameters). **Only the `+ Sync` on the THREE
`Arc<dyn EnvSource + Send + Sync>` sites is now redundant** — `EnvSource: Sync`
is a supertrait and says nothing about `Send`, so dropping `+ Send` would change
the type and stop the `Arc` being movable across threads. Making impls harder
and leaving existing consumers legal is the one thing a supertrait *can* be
relied on to do.

The reasoning before this run was
"a supertrait makes impls harder and consumers easier, so this should compile" —
and *should*, inferred from a grep, is the exact shape of reasoning that has
been wrong repeatedly on this programme. `cargo check` across all three crates
and all targets says it does compile. That is a different kind of claim.

**`cargo xtask fmt` was failing, and nothing had ever run it on this tree.**
`src/lib.rs`, `src/paths.rs` and `tests/coord_config_blank_settings.rs` — all
`roost-host`, all import wrapping, all from the XDG-state-host work. The drift
was invisible because every commit since had been checked by a gate scoped to
some other crate. Same shape as the item above it: **the instrument existed and
nobody pulled it.** Formatted with `cargo fmt -p roost-host` rather than
`--all`, which ignores `workspace.exclude` and would reformat vendored
`third_party`; the change is 8 insertions and 9 deletions of whitespace, and
`cargo check -p roost-host --all-targets` is clean after it.

Ratchets on this tree, for the ladder below: `AwaitingDomainPort` **27**, worker
`UNIMPLEMENTED` **6** on `v3` against **2** on `v3-worker` @ `57bd7f74`, `todo!`
**0**.

**None of the above is a stack booting.** The Phase 2, Phase 3 and Phase 6 gates
have not run on this tree and nothing here should be read as predicting them.

**MERGE POINT MEASURED, on the real tree rather than a projection.** A trial
merge of all five tracks into `v3` at `a41f9d38` — **zero conflicts at every step**,
`v3-cli`, `v3-cli-cutover`, `v3-coord`, `v3-worker`, `v3-web`:

```
xtask: checked 2744 inputs   unreached 0   violations 16
  14  size — files stay <=400 lines          (8 roost-client-core, 6 roost-web-terminal)
   2  tests — a fixture reached from a binary that does not declare the allow
            roost-web-terminal/tests/mouse_forwarding.rs  and  mouse_reporting.rs,
            both compiling the shared `mouse_forwarding_support` fixture
```

**THE LADDER, each rung measured rather than inferred:**

|tree|inputs|violations|what that merge cleared|
|---|---:|---:|---|
|`v3`|2111|10|the baseline: 2 roost-cli size, 1 keeper lint table, 7 keeper fixture allow|
|`v3` + `v3-cli`|2124|**8**|**the 2 roost-cli size violations — the CLI merge alone clears exactly those**|
|`v3` + `v3-cli` + `v3-worker`|2425|**0**|the keeper lint table and the 7 fixture allows|

**So both merges are needed and the order is forced: `v3-cli` first because it
clears two, then `v3-worker` because it clears the other eight.** The two rows
above the full merge were re-measured tonight; the middle one was already on
record at `20e56d49` and **I re-derived it instead of grepping for it — the tenth
instance of the same slip, and the one where the document already had the answer
and I spent a turn and two failed commands producing it again.**

**`v3`'s own ten are GONE, and that is the merge-order prediction confirmed.** The
two `roost-cli` size violations cleared with `v3-cli`; the `roost-keeper/Cargo.toml`
lint table and the seven keeper `fixture_allow` findings cleared with `v3-worker`.
**Nothing is inherited from `v3` any more.** What remains, by file, named:

|file|lines|rule|owner|
|---|---:|---|---|
|`roost-client-core` (8 files)|401–477|size|web|
|`roost-web-terminal` (4 files)|401–477|size|web|
|`roost-web-terminal/tests/mouse_forwarding.rs`|—|fixture allow|web|
|`roost-web-terminal/tests/mouse_reporting.rs`|—|fixture allow|web|
|**`roost-coord/tests/event_publication.rs`**|**406**|size|coordinator|
|**`roost-worker/src/session/respawn.rs`**|**438**|size|**worker**|

**Fourteen are the web track's, and TWO ARE NOT** — the coordinator's
`event_publication.rs` at 406, which it is already blocked on, and the worker's
`respawn.rs` at 438, which it does not know about. **An earlier revision of this
entry said "all sixteen are the web track's", and that was an over-correction: I
had just fixed the FIXTURE attribution by reading the named files, and then
re-derived the SIZE attribution from the grouped count in the same breath. Two
corrections in a row, the second undoing the first's spirit.** The rule prints the
file; read the file.

**An earlier reading of this same sweep put one fixture allow on `roost-worker` and
one on `roost-coord`. That was wrong, and it was wrong the same way as most of the
mistakes in this file: the per-crate summary was read as an attribution when the
finding names its own files, and the files say `roost-web-terminal` twice.** The
correct attribution is the one the rule prints per violation, not the one a grouped
count suggests.

**`unreached 0` across the whole merge** — every module in every crate is
registered, which is the property the unreached-module rule exists to protect and
the first time it has been checked on a merged tree.

*No Phase 2, 3 or 6 result is recorded yet.* Stage 3 has not run. The three gates, in order:

1. **Phase 2** — `ROOST_SMOKE_WORKER_EXECUTABLE=<release>/roost bun run test:terminal`.
   The load-bearing spec is `terminal-delivery.spec.ts` *"browser smoke flow
   creates and cleans its resources"*.
2. **Phase 3** — the same with `ROOST_SMOKE_COORD_EXECUTABLE` alone, then with both
   variables. **The "both" run is the stack production will run in Stage 4.**
3. **Phase 6 install** — the scratch `roost3gate` user, browser pairing, the
   keeper PID across a deploy, and the import check.

### All-Rust oracle runs, watched

Every row is a run someone watched, with its artifacts named. From 2026-10-01
every run goes through `bun smoke/parity/run.ts` (CLAUDE.md `### Commands`),
whose pin manifest is the artifact identity; the earlier rows are the hand-run
measurements of the three 2026-09-30 sessions. Counts are passed / failed /
skipped.

|when|stack and pass|command|artifacts|log|result|
|---|---|---|---|---|---|
|2026-09-30 14:53|Rust, `terminal-peer.spec.ts` (chromium-desktop)|`playwright test` on the pin|not recorded|—|5 failed: `:62` died in its fixture (`waitForFunction(workers[fp])`, 89 s); `:97`/`:300` `direct route unavailable … "failureDetail":"negotiation deadline"`|
|2026-09-30 14:55|Rust, main|`bun run test:terminal`, three `ROOST_SMOKE_*` knobs|not recorded|`~/.cache/rust-suite-20260930-1455.log`|57 / 60 / 28 (serial never ran: the profile stopped on the red main pass)|
|2026-09-30 15:33|Rust, main|same, after the second session's fixes|`roost` `02981d31…`|`~/.cache/rust-suite2-20260930-1533.log`|56 / 61 / 28|
|2026-09-30 15:53|**Bun** (all-TS), both|`bun run test:terminal`, knobs unset|`v3` @ `1f1b6096` + uncommitted|`~/.cache/bun-suite-20260930-1553.log`|main 142 / 0 / 3; serial 14 / 1 / 3 — the 1 is `terminal-peer-perf.spec.ts:7`|
|2026-09-30 ~16:00|Rust, `terminal-peer.spec.ts:62`|`playwright test`|`roost` `02981d31…`, `roost-web-dxha0a3e2d6a08ed146.js`|—|fixture passes after local-bootstrap priming; fails one layer down at `waitForDirectRoute(…,"loopback")`: `activeKind` sync, `peerPhase` idle|
|2026-09-30 17:35|Rust, main|`bun run test:terminal`, after direct-route link 1|not recorded|`~/.cache/rust-suite3-20260930-1735.log`|56 / 61 / 28; 53 Rust-red/Bun-green, 13 Rust-only skips, both-red 0|
|2026-09-30 17:54|Rust, serial|`bunx playwright test --project chromium-serial --reporter=line`|not recorded|`~/.cache/rust-serial-20260930-1754.log`|4 / 10 / 4|
|2026-10-01|Rust, serial|`bun smoke/parity/run.ts suite --stack rust --pass serial --label phase0-serial-check`|pin `1f1b6096`+dirty, `roost` `1fa72d84…`, `roost-web-dxhe0dd67381eea45f.js`|`test-results/parity/rust-phase0-serial-check.run.json`|5 / 9 / 4|
|2026-10-01 18:11|Rust, both|`bun smoke/parity/run.ts suite --stack rust --pass both --label landed`|`pin d5bd76c752b0 roost=541e9422dbf7 keeper=86d50b2fcfff web=roost-web-dxh1341e6fcbbd04976.js,roost-web_bg-dxhe57c31864ed24bbf.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-landed.run.json`|main 77 / 40 / 28 (864 s); serial 5 / 9 / 4 (460 s)|
|2026-10-01 18:33|**Bun** (all-TS), both|`bun smoke/parity/run.ts suite --stack bun --pass both --label baseline`|source `d5bd76c7`, clean|`gate-evidence/parity/bun-d5bd76c7.run.json`|main 141 / 1 / 3 (615 s); serial 15 / 0 / 3 (1440 s) — the 1 is `terminal-frame-repair.spec.ts:151`|
|2026-10-02 03:58|Rust, main|`bun smoke/parity/run.ts suite --stack rust --pass main --label mid1 --allow-stale`|`pin 581c1b41 roost=f04a3b68828c keeper=86d50b2fcfff web=roost-web-dxh20fe6c4eb777d945.js,roost-web_bg-dxhccec88e997eba05d.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-mid1.run.json`|main 100 / 17 / 28 (647 s); verdict gap 17, both-red 0, rust-skip-only 25, both-skip 3, green 100|
|2026-10-02 07:02|Rust, main|`bun smoke/parity/run.ts suite --stack rust --pass main --label mid2`|`pin 89d9a730 roost=4240676b41ec keeper=86d50b2fcfff web=roost-web-dxh3a94f2b69a34745d.js,roost-web_bg-dxhe166ff5825931da2.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-mid2.run.json`|main 103 / 14 / 28 (654 s); verdict gap 14, both-red 0, rust-skip-only 25, both-skip 3, green 103|
|2026-10-02 07:18|Rust, serial|`bun smoke/parity/run.ts suite --stack rust --pass serial --label mid2s`|`pin 89d9a730 roost=4240676b41ec keeper=86d50b2fcfff web=roost-web-dxh3a94f2b69a34745d.js,roost-web_bg-dxhe166ff5825931da2.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-mid2s.run.json`|serial 10 / 4 / 4 (255 s)|
|2026-10-02 10:09|Rust, main|`bun smoke/parity/run.ts suite --stack rust --pass main --label mid3`|`pin de0372bf roost=509b0ffe373a keeper=11c5d0cd57c0 web=roost-web-dxh3beae81f5f66bfe1.js,roost-web_bg-dxhd94b34d2233a26bc.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-mid3.run.json`|main 110 / 7 / 28 (620 s); verdict gap 7, both-red 0, rust-skip-only 25, both-skip 3, green 110|
|2026-10-02 10:26|Rust, serial|`bun smoke/parity/run.ts suite --stack rust --pass serial --label mid3s`|`pin de0372bf roost=509b0ffe373a keeper=11c5d0cd57c0 web=roost-web-dxh3beae81f5f66bfe1.js,roost-web_bg-dxhd94b34d2233a26bc.wasm features=[] web-features=[smoke]`|`test-results/parity/rust-mid3s.run.json`|serial 11 / 3 / 4 (218 s) — `perf.spec.ts:298` chromium-serial, `perf.spec.ts:302` chromium-serial, `terminal-peer-perf.spec.ts:7` chromium-serial|
|2026-10-02 15:29|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label mid4`|`pin f9abaa7c roost=4611dfb093bd keeper=11c5d0cd57c0 web=roost-web-dxhad804e96c61a47.js,roost-web_bg-dxhdfee8d40584dea.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-mid4.run.json`|main 141 / 1 / 3 (959 s) — `terminal-peer-failover.spec.ts:377` firefox-peer; serial 15 / 0 / 3 (1669 s); verdict gap 1, both-red 0, rust-skip-only 0, both-skip 6, green 156|
|2026-10-02 16:57|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin e03378e5 roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxhd79426d9bcff20ad.js,roost-web_bg-dxh84b943565f9daa8a.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-e03378e5.run.json`|main 142 / 0 / 3 (926 s); serial 14 / 1 / 3 (995 s) — `terminal-peer-perf.spec.ts:7` chromium-serial; verdict gap 1, both-red 0, rust-skip-only 0, both-skip 6, green 156|
|2026-10-02 19:40|Rust, both|`bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin 8024b50e roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxhfc61a8adacb99c64.js,roost-web_bg-dxh913888b286ca7656.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-8024b50e.run.json`|main 141 / 1 / 3 (987 s) — `terminal-delivery.spec.ts:130` chromium-desktop; serial 15 / 0 / 3 (1650 s); verdict gap 1, both-red 0, rust-skip-only 0, both-skip 6, green 156|
|2026-10-02 21:34|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin d0548f47 roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxhfbeb43de13f46d51.js,roost-web_bg-dxh367b338c8ab49711.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-d0548f47.run.json`|main 140 / 2 / 3 (848 s) — `terminal-frame-repair.spec.ts:151` chromium-desktop, `terminal-peer.spec.ts:97` firefox-peer; serial 15 / 0 / 3 (1543 s); verdict gap 1, both-red 1, rust-skip-only 0, both-skip 6, green 155|
|2026-10-02 23:08|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin c1310f27 roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxh3e76822d50a37990.js,roost-web_bg-dxh3d4d50ae4326443.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-c1310f27-run1.run.json`|main 141 / 1 / 3 (824 s) — `composer-mobile-keyboard.spec.ts:9` chromium-desktop; serial 15 / 0 / 3 (1412 s); verdict gap 1, both-red 0, rust-skip-only 0, both-skip 6, green 156|
|2026-10-03 00:05|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin c1310f27 roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxh3e76822d50a37990.js,roost-web_bg-dxh3d4d50ae4326443.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-c1310f27-run2.run.json`|main 141 / 1 / 3 (845 s) — `attachment-direct.spec.ts:149` chromium-desktop; serial 15 / 0 / 3 (1464 s); verdict gap 1, both-red 0, rust-skip-only 0, both-skip 6, green 156|
|2026-10-03 00:49|Rust, both, production-shape `roost`|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label plain`, after `run.ts build --plain --no-web`|`pin c1310f27 roost=a970046c2bba keeper=11c5d0cd57c0 web=roost-web-dxh3e76822d50a37990.js,roost-web_bg-dxh3d4d50ae4326443.wasm features=[] web-features=[smoke] profile=release`|`test-results/parity/rust-plain.run.json`|main 116 / 1 / 28 (461 s) — `composer-mobile-keyboard.spec.ts:9` chromium-desktop; serial 14 / 0 / 4 (1462 s) — every extra skip carries the packaged-worker fault-controls reason|
|2026-10-03 01:34|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin c1310f27 roost=2807354fd57a keeper=11c5d0cd57c0 web=roost-web-dxh3e76822d50a37990.js,roost-web_bg-dxh3d4d50ae4326443.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final-c1310f27-run3.run.json`, archived as `gate-evidence/parity/rust-c1310f27.run.json`|main 142 / 0 / 3 (842 s); serial 15 / 0 / 3 (1526 s); verdict gap 0, both-red 0, rust-skip-only 0, both-skip 6, green 157|
|2026-10-03 04:00|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label keeper`|`pin f2e0a386 roost=2807354fd57a keeper=63d404562b88 web=roost-web-dxh3e76822d50a37990.js,roost-web_bg-dxh3d4d50ae4326443.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-keeper.run.json`|main 142 / 0 / 3 (782 s); serial 15 / 0 / 3 (1463 s); verdict gap 0, both-red 0, rust-skip-only 0, both-skip 6, green 157; 0 test keepers alive 35 s after|
|2026-10-03 14:05|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final`|`pin 33db0453c998 roost=006570f5e19f keeper=3908338b45e0 web=roost-web-dxh304afda2ad10308f.js,roost-web_bg-dxhb02179b03d1ad6c4.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-final.run.json`, archived as `gate-evidence/parity/rust-33db0453.run.json`|main 142 / 0 / 3 (673 s); serial 15 / 0 / 3 (1356 s); verdict gap 0, both-red 0, rust-skip-only 0, both-skip 6, green 157|
|2026-10-03 14:40|Rust, both|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass both --label final-2`|`pin 33db0453c998`, the same artifacts|`test-results/parity/rust-final-2.run.json`|main 141 / 1 / 3 (691 s) — `terminal-predictive-echo.spec.ts:51` chromium-desktop; serial 15 / 0 / 3 (1408 s); 0 test keepers alive right after|
|2026-10-03 15:20|Rust, main|`roost-box exclusive bun smoke/parity/run.ts suite --stack rust --pass main --label final-3`|`pin 33db0453c998`, the same artifacts|`test-results/parity/rust-final-3.run.json`|main 142 / 0 / 3 (669 s); 0 test keepers alive right after|
|2026-10-04 01:18|Rust, both, box shared with another session|`bun smoke/parity/run.ts suite --stack rust --pass both --label perf`|`pin ae70f26de8cd roost=bb4943f52b30 keeper=5e3b2750f547 web=roost-web-dxh12f0bfe5e0d5f462.js,roost-web_bg-dxhf6a2c05f74732362.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-perf.run.json`|main 148 / 0 / 3 (934 s); serial 15 / 0 / 3 (1404 s); verdict gap 0, both-red 0, rust-skip-only 0, both-skip 6 (the same six), green 163|
|2026-10-04 03:01|Rust, serial, started at load 2.27 with no rustc running|`bun smoke/parity/run.ts suite --stack rust --pass serial --label perf-serial-quiet`|`pin ae70f26de8cd`, the same artifacts|`test-results/parity/rust-perf-serial-quiet.run.json`|serial 15 / 0 / 3 (1376 s)|
|2026-10-04 06:12|Rust, both, TypeScript product tree deleted|`bun smoke/parity/run.ts suite --stack rust --pass both --label ts-deleted`|`pin f3284597d59f roost=535e0b774c58 keeper=ccd7bb11cf52 web=roost-web-dxh12f0bfe5e0d5f462.js,roost-web_bg-dxhf6a2c05f74732362.wasm features=[smoke] web-features=[smoke] profile=release`|`test-results/parity/rust-ts-deleted.run.json`|main 148 / 0 / 3 (936 s); serial 15 / 0 / 3 (1362 s); verdict against `bun-d5bd76c7` gap 0, both-red 0, rust-skip-only 0, both-skip 6 (the same six), green 163|

**The landed tree's verdict** (`run.ts verdict rust-landed.run.json
gate-evidence/parity/bun-d5bd76c7.run.json`, keyed by file, title and project):
gap **48**, both-red 1 (`terminal-frame-repair.spec.ts:151`, green on Bun the day
before), rust-skip-only **26** (the 13 peer-fault cases on each of their two
projects), both-skip 6 (exactly the six named skips), green 82, bun-only 0,
skew 0. `ui-layout-apply.spec.ts:119` is green, and no report in either Rust
pass mentions the browser tab fence: the `ui_state` fix held. This verdict is the
Phase 2 worklist.

**LANDING GATE — GREEN on the tree of `0814d5a0`, 2026-10-01.** The 2026-09-30
sessions' work plus the `ui_state` fence removal and two cherry-picks, gated once
as a whole with nothing else running, `CARGO_BUILD_JOBS=8`, target `target/`:

|criterion|result|
|---|---|
|`cargo xtask fmt`|exit 0|
|`ROOST_REPO_ROOT=$PWD cargo xtask lint`|**0 violations, 6110 inputs**|
|`cargo clippy --workspace --all-targets -- -D warnings`|exit 0|
|`cargo test --workspace --no-fail-fast`, two runs|769 binaries, **5273 passed / 0 failed**, both runs|
|`cargo test -p roost-web --features smoke`|69 binaries, **816 passed / 0 failed**|
|CI wasm32 build (`ci.yml` step "wasm32 build")|exit 0, 0 warnings|
|`dx build --release -p roost-web --platform web --features smoke`|built|
|vendored terminal core suite|132 + 45 + 8 + 1 passed|

**The final verdict** (`run.ts verdict gate-evidence/parity/rust-c1310f27.run.json
gate-evidence/parity/bun-d5bd76c7.run.json`): gap **0**, both-red 0, rust-skip-only **0**,
both-skip 6 (exactly the six named skips), green 157, bun-only 0, skew 0. Green is 157 rather
than Bun's 156 because `terminal-frame-repair.spec.ts:151`, red in the Bun baseline, is green
here. The thirteen peer-fault cases run on Rust on both of their projects.

**FINAL GATE — GREEN on `c1310f27`, 2026-10-03.** The tree `v3` carries once every parity
track had merged. Release pin `roost=2807354fd57a keeper=11c5d0cd57c0
web=roost-web-dxh3e76822d50a37990.js features=[smoke] profile=release`. The suite ran with the box
held (`roost-box exclusive`); the cargo gates ran as one script with `CARGO_BUILD_JOBS=4`, target
`target/`:

|criterion|result|
|---|---|
|all-Rust oracle, both passes|main 142 / 0 / 3 (842 s), serial 15 / 0 / 3 (1526 s), the verdict above|
|`cargo xtask fmt`|exit 0|
|`ROOST_REPO_ROOT=$PWD cargo xtask lint`|**0 violations, 6242 inputs**|
|`cargo clippy --workspace --all-targets -- -D warnings`|exit 0|
|the same clippy over `roost-web`, `roost-worker` and `roost-cli` with their `smoke` features|exit 0|
|`cargo test --workspace --no-fail-fast`, two runs|781 binaries, **5338 passed / 0 failed / 16 ignored**, both runs|
|`cargo test -p roost-web --features smoke`|74 binaries, **842 passed / 0 failed**|
|`cargo test -p roost-worker -p roost-cli --features roost-worker/smoke,roost-cli/smoke`|239 binaries, **1469 passed / 0 failed**|
|vendored terminal core suite|187 passed|
|wasm32: `roost-protocol` + `roost-client-core`, then `roost-web`|exit 0, 0 warnings|
|`cargo build --release -p roost-cli -p roost-keeper`|exit 0|
|`bun x tsgo -p tsconfig.base.json --noEmit`; `bun run lint`|exit 0; 0 violations|

**Production shape, same tree.** `run.ts build --plain --no-web` pinned a `roost` built without
the `smoke` feature (`roost=a970046c2bba`) beside the smoke bundle, and the suite ran both passes on
it (row `plain` above). Every extra skip — 25 in main, 1 in serial, the thirteen fault cases on
their two projects — carries the packaged-worker reason "terminal peer fault controls require a
source worker", and the one red is the first flake below. The two fault-socket flags occur 0 times
in that binary and once in the smoke pin's. `run.ts build --plain` (no `smoke` anywhere) produced
`roost-web-dxheda24428c317892.js` with **0** `__smoke` occurrences across the uncompressed bundle,
against 13 in the smoke bundle.

**Two flakes, one red each in the three full runs on this tree; neither is Rust-only
behaviour.** Runs 1 and 2 above each had exactly one red; run 3 had none.

- `composer-mobile-keyboard.spec.ts:9`, at `terminal-probe-helpers.ts:95` (the grid epoch moved
  under a held selection). Coming back from the file preview, the pane publishes twice on BOTH
  stacks: Rust always 25 then 28 rows; Bun 32 then 28, or 28 then 28, in two of three sampled runs.
  The spec's "before" probe can land between the two baselines; in the red trace the second baseline
  arrives at +97 ms and the probe reads at +68 ms. Isolated `--repeat 20`: Rust 1 red on `c1310f27`,
  0 on `d0548f47`; Bun 0, though Bun failed the same assertion once with an in-page probe attached.
  It is also the plain run's one red.
- `attachment-direct.spec.ts:149`, at `:168` (`requests.relay` 0). Playwright's server runs in the
  test process, so the spec's synchronous 700 KB `toEqual` (1.6–1.8 s) holds every browser event
  until it ends. The `AttachFileChunk` POST's `request` event normally arrives 115–170 ms after the
  POST starts. Rust lists the stored file on the first poll and reached the comparison 117 ms after
  the POST in the red run (209 ms in a green one; Bun 285 ms), so the event landed behind the
  comparison and the trace records it 1.9 s after its browser-side start. Isolated `--repeat 30`:
  Rust 1 red, Bun 0.

Neither was fixed on `c1310f27`. The first was v2's own reveal behaviour, reached sooner; the second
was timing inside the oracle. Both are fixed on `33db0453`, below, the second with an oracle edit
that `main` carries too.

**Keeper lifecycle, re-gated on `f2e0a386`.** Cleaning up after the runs above found 1155
`roost-keeper` processes still alive (3.1 GB RSS), every one on a deleted test socket, where the
Bun stack leaves none: the Rust keeper ignored SIGTERM and never noticed its socket go, and the
oracle's teardown cannot authenticate to it (FAILURE-INDEX "A keeper outlives its deleted socket and
ignores SIGTERM"). The same batch on each side (`run.ts spec terminal-delivery.spec.ts
terminal-peer.spec.ts --project chromium-desktop`, 9 passed both times) left 16 test keepers alive 35 s
later on the `c1310f27` pin and 0 on the `f2e0a386` pin, whose `roost` is byte-identical — only
`roost-keeper` changed. The tree was then re-gated: the suite (row `keeper` above, gap 0, 157 green,
no test keeper alive 35 s after it), and the workspace gates — fmt, clippy with and without the
`smoke` features, `xtask lint` 0 violations / 6246 inputs, `cargo test --workspace` **5342 passed /
0 failed / 16 ignored** twice (782 binaries), `roost-web` smoke 842, worker + CLI smoke 1469,
vendored 187, wasm32 0 warnings, the release build, and `bun run lint` 0 violations.

**FINAL GATE — GREEN on `33db0453`, 2026-10-03: the keeper authenticates, and both flakes are
fixed.** Four commits on `655f5938` — `5bb37805` (keeper), `de5907d1` (worker), `b6afc643` (web),
`8ba13223` (smoke) — and a merge of `main`'s copy of the last (`48f6fa2f`), which changes no file.

- *Keeper teardown.* v2's own client (`resolveLocalEndpoint` + `connectKeeperAuthenticated` +
  `shutdownKeeperAuthenticated`) against the pre-change release keeper: `authenticated=false
  compatible=false`, `shutdown=false`, the keeper still running. Against `keeper=db646ec2c0d3`
  (pin `47ab5976`): `authenticated=true compatible=true pid=<keeper pid>`, `shutdown=true`, the
  keeper gone 23 ms later; one started with a different capability file refuses and stays up. The
  batch above (`terminal-delivery.spec.ts terminal-peer.spec.ts`, chromium-desktop) passed 9 and left
  0 test keepers alive right after, where `c1310f27` left 16 alive 35 s later; runs `final-2` and
  `final-3` left 0 right after the suite.
- *Mobile reveal.* The reveal published 25 then 28 rows — `terminal view opened` rows 25, then
  `terminal view resized` 28 at +68 ms, the slot's inline height 455 px then 503 px. The compact
  slot now takes its box from the deck's (`height: auto; bottom: 0px`), so the reveal publishes
  once, at 28 rows. Removing that resize exposed a second cause, the worker link writing a reopened
  stream's baseline ahead of the view-state that announces it: 3 of 20 traced runs then failed
  "terminal stream probe omitted a current worker/coordinator sequence" (FAILURE-INDEX "A worker
  link writes a reopened view's baseline ahead of the view-state that announces it"). With both
  fixes, `--repeat 20` passed 20 twice, and a traced `--repeat 20` showed one `terminal view
  opened` at 28 rows and no resize within 500 ms in every run.
- *Attachment fallback.* `:168` polls `requests.relay`; `--repeat 30` passed 30.

Run `final-2` had one red, `terminal-predictive-echo.spec.ts:51` "no prediction was painted — the
case proves nothing": about 0.9 s after the view opened, a snapshot request from the browser side
re-seeded the pane mid-burst (pty-fixture worker "a snapshot request re-baselined every sink",
coordinator `terminal.screen_seed`, no coordinator-side resync logged). Its trace was lost to the
serial pass's output cleanup, so the trigger is not identified. Isolated on the same pin,
`--repeat 30 --trace` with four workers passed 30, and `final-3`'s main pass was green.

|criterion|result|
|---|---|
|all-Rust oracle, both passes|main 142 / 0 / 3 (673 s), serial 15 / 0 / 3 (1356 s); verdict gap 0, both-red 0, rust-skip-only 0, both-skip 6, green 157, bun-only 0, skew 0|
|`cargo xtask fmt`|exit 0|
|`ROOST_REPO_ROOT=$PWD cargo xtask lint`|**0 violations, 6253 inputs**|
|`cargo clippy --workspace --all-targets -- -D warnings`|exit 0|
|the same clippy over `roost-web`, `roost-worker` and `roost-cli` with their `smoke` features|exit 0|
|`cargo test --workspace --no-fail-fast`, two runs|783 binaries, **5354 passed / 0 failed / 16 ignored**, both runs|
|`cargo test -p roost-web --features smoke`|74 binaries, **843 passed / 0 failed**|
|`cargo test -p roost-worker -p roost-cli --features roost-worker/smoke,roost-cli/smoke`|239 binaries, **1470 passed / 0 failed**|
|vendored terminal core suite|187 passed|
|wasm32: `roost-protocol` + `roost-client-core`, then `roost-web`|exit 0, 0 warnings|
|`cargo build --release -p roost-cli -p roost-keeper`|exit 0|
|`bun x tsgo -p tsconfig.base.json --noEmit`; `bun run lint`|exit 0; 0 violations|

### Where tonight's numbers live, since this file is long

Everything measured on 2026-09-27 while the tracks were running is in the
sections above, and each says which tree it was measured on:

- **CLOSED, not EXPECTED RED** — the deferred-reap reachability guard is **green on
  both conjuncts** at `8880699f` (`--test event_publication`, 6 passed / 0 failed,
  against 5/1 before). There is **no deliberately red test** in this file any
  more; three records said otherwise until 2026-09-28, each dating the fix to R4
  when R4 is a different property. See the CLOSED section above.
- **The gate ratchets** — on `v3` @ `dbf0edd2`: `AwaitingDomainPort` 27,
  `UnwiredInV2` 16, `UNFINISHED` 3 (all three `#[ignore]` attributes, in
  `push_sender_bounds.rs:175` and `sync_v2_send_queue.rs:215,257`), `todo!` 0.
  Worker `UNIMPLEMENTED`: **6 on `v3`**, **2 on `v3-worker` @ `57bd7f74`** — both
  measured, and the distance between them is the worker track landing. The
  detail, with the file and line of each marker on each tree, is in the worker's
  own entry above.
- **`xtask lint` on `v3`** — all ten violations enumerated, and what each track's merge clears.
- **The unreached-module rule** — the guarded sweep, its canary, and the eight files it found on `v3-web`.
- **The import check** — why it recomputes its counts and restates the fingerprint filter in its own SQL.
- **The instruments** — four findings whose common shape is a check that could not see the thing it claimed to check.
- **The CLI cutover gate** — 399/0/0 twice at `a13c385d`, why two agreeing logs
  are not one log twice, `import-v2`'s first execution, and the flake those two
  green runs do not disprove.

## Perf numbers worth not regressing

From the baseline's own instrumentation, since the perf specs assert against
their own budgets and a silent 2× drift is easy to miss in a pass count:

| Case | Baseline |
|---|---|
| `history_scroll_plan` | 8 steps, 250-row pager fetch, 500-row ahead |
| `history_scroll_latency` | median 178ms, max 293ms per step, 4 total RPCs, 30s budget |
| navigation → first paint | `cold_driver_to_paint_ms` 914, fresh navigation 3.0–3.7s |
| 20k flood | 306ms wall, 0 dropped phases, 19,970 retained, 259 DOM nodes, 32 cell rows |
| bounded deck (16 spawned) | 9 mounted slots, reveal p50 47ms, p95 57ms |
| peer perf (Sync vs loopback vs direct) | asserted by `terminal-peer-perf.spec.ts:7` |

On `ae70f26d` (brotli wasm, early-exit ICE, frame-only revision, dirty-row deltas, opt-level 3
native), quiet serial pass above: cold driver-to-paint 516 ms, 20k flood 121 ms wall with 0
dropped phases, 19,970 retained, 250 DOM nodes, 32 cell rows, 0 long tasks; history scroll
(main pass) median 91 ms, max 162 ms, 4 RPCs. The wasm goes out as 1,613,266 B of brotli
against 5,969,523 B raw. With a real STUN server (`stun:stun.l.google.com:19302`, chromium)
`time_to_direct_ms` was 741/799/781 and the worker's negotiating→established gap 241–249 ms,
where this host's installed worker logged 3,007–3,011 ms. The fleet "delayed worker-link"
together-typing p50 (261–328 ms) sits above `33db0453`'s 218 ms; a serial pass on the same tree
with those five commits reverted (`9b9817fe`, label `ab-revert`) measured 327 ms and the
two-worker case 107/129 ms p50/p95 against 108/129 with them, so that drift is not theirs.
Two serial passes on `33f0aef5` (the same runtime code, pin `bb4943f52b30`, labels `ab-head`
and `ab-head2`, started at load < 4 with no rustc running) bracket that revert run:
delayed-fleet together p50/p95 328/434 ms and 265/341 ms. Within run variance; the five
commits stay.

On `0b71c31b` (route cache before SQLite, outbox drained during appends, one flush per
egress batch, one proto conversion per frame, direct frames listener, boot-time door probe,
shared DTLS certificate and gathering cache, ETag/304), measured on a scratch stack on this
host: debug native binaries, the wasm-release bundle, and headless chromium. Over Sync with
the peer carrier disabled, 35 input batches each had `route_ms` 0 and `sent_ms` 0;
`settled_ms` was p50 2 ms and max 3 ms, and `audit_ms` max 17 ms. The one durable append
during a concurrent spawn took `append_ms` 3. Over the direct carrier on a loopback page whose
door was unusable, `time_to_direct_ms` was 858/645/641 ms. The phase entries, as
gathering/negotiating/authenticating/candidate/active ms from the attempt start, were
0/241/491/741/758, 0/69/318/318/580 and 0/80/330/580/595. The worker logged
`certificate_ms` 0 for every peer and `gather_ms` 224–241. Most of that is the 200 ms
`REFLEXIVE_SETTLE` window, so the gathering cache does not shorten it. A 20k `seq` flood on
the direct carrier took 267 ms from Enter to `20000` painted (polled every 20 ms; not the
smoke harness's method), with no paint refusal and no long task. A reload's first
contentful paint was 140–144 ms, with the wasm served from the immutable cache and
`index.html` revalidated by ETag. A loopback page makes one `/api/local-bootstrap` request on
its origin and one door probe per load; a tailnet-origin page makes none on its origin. The
wasm goes out as 1,551,572 B of brotli against 5,744,197 B raw.

On `88907942` (STUN probed only from each server's egress socket and never toward a server
with no route, the offer read when the reflexive settle lapses, a drained promotion claimed on
its settling result), same host and harness: `time_to_direct_ms` was 395/373/382 ms, with
phases 0/58/58/308/326, 0/68/68/68/306 and 0/64/64/64/316. The worker logged `gather_ms`
41/21/24 (was 224–241; the v6 STUN address has no route here and a tailnet v6 socket used to
hold the settle), `ready_ms` 46/25/29 from the offer, `connected_ms` 12–13 and
`channel_open_ms` 15–16 from the answer. Authenticating→candidate is now 0 on two of three;
the remaining ~240 ms is candidate→active, which this pass did not touch. An idle visible pane
was resynced at 10, 20, then every 30 s (was every 5 s), with no `foreground terminal stall`
warn in 150 s. After a coordinator restart with the peer disabled and a reload, the 12
keystrokes refused `terminal input route changed` (`written_bytes` 0) were all re-sent after
the Sync claim and accepted, and the screen showed `echo MARK2` and `MARK2`.

On `2be381d8` (every transport observation stamps its own instant, the browser offer read at the
first server-reflexive candidate, a refused frame asking for its baseline in the same dispatch),
same host and harness: `time_to_direct_ms` was 164/177/195 ms, with phases 0/43/96/110/121,
0/49/102/114/123 and 0/55/107/119/131. The stamps are now distinct and monotonic; the
`88907942` equal triples were the machine's clock left at the last sweep, not real phases.
Negotiating is one STUN round trip plus the offer read (was ≈ 250 ms with the 200 ms settle),
and candidate→active is 9–12 ms (the ~240 ms there was the stale stamp). Demand→attempt start,
the grant wait before `OpenTransport` (`time_to_direct_ms − active_ms`; `gathering_ms` is 0 by
definition), was 43/54/64 ms. The worker logged `gather_ms` (`elapsed_ms`) 24/27/26, `ready_ms`
29/32/30 from the offer, `connected_ms` 14/11/12 and `channel_open_ms` 18/16/16 from the answer.

On `4d6ae136` (the peer transport opens in the dispatch that requests the grant, on the STUN
servers `AuthCoordIdentity` advertises; the door answer is reported at mint time; the offer is
held until the grant lands), same host and harness, three reloads 8 s apart:
`time_to_direct_ms` 241/168/168 with phases 0/143/212/227/241, 0/88/141/152/168 and
0/92/143/155/168. The grant wait before `OpenTransport` (`time_to_direct_ms − active_ms`) is 0
on all three (was 43/54/64). The transport opened 12 ms after the view and 49 ms before the
mint returned (second reload: view 126, open 138, mint 187, first reflexive candidate 214, answer 266,
all ms from the page's first console line), so no offer was held. Negotiating is now gated by
the STUN round trip (≈ 75 ms to the first reflexive candidate), not by the mint, which is why the
total moved by tens of ms rather than by the whole former wait. The worker logged `elapsed_ms`
40/27/26, `ready_ms` 46/32/31 and `connected_ms` 13/12/12. Reload timeline, ms from
navigation (`window.__roostPhaseTimeline()`): `module_start` 121/113/111, `sync_subscribed`
203/159/157, `terminal_mount` 326/250/247, `first_cell_apply` 404/416/296. The protected
surface renders only once the terminal snapshot publishes (`Gate::Checking` until
`mark_protected_snapshot_published`), so `terminal_mount` follows the snapshot and a pane cannot
mount from the visit memory earlier without changing that gate. `seq 1 20000` over the peer
painted its last line with no overflow or refusal. `panic = "abort"` in `[profile.wasm-release]`
changed the bundle from 5 767 218 to 5 768 393 bytes raw and 1 559 075 to 1 559 364 brotli
(`wasm32-unknown-unknown` already aborts on panic), so the profile does not set it.

Retained-marker bounds are worth keeping in view because they are the
history-corruption tripwire: a Rust renderer that drops the retained floor
will pass every functional spec and still lose scrollback.
