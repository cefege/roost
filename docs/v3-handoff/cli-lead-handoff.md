# Track L lead handoff — `roost` CLI

**Read the worktree and `git log`, not this note.** This is a moment, not a state.
The tree is at **`7bb4b0e4`, PUSHED** (`origin/v3-cli` == HEAD, tree clean).

Worktree `/home/almalinux/repos/roost-v3-cli`, branch `v3-cli`. Leads in order:
`CliLeadL`, `CliLeadL2`, `CliLeadL3`, `CliLeadL3b` (current).

`7bb4b0e4` carries the three paths `CliLeadL3` left uncommitted — the only
uncommitted work on any track when this lead started, and **the first thing
this lead did, before any cargo command**:

1. `FleetRuntime::new`'s `prior_sha: &str` parameter deleted at the definition
   (`push/runtime.rs:86`) and the call (`push/command.rs`). The struct has no
   such field; it reads `self.plan.prior_sha` at `runtime.rs:67,127,271-273`
   and the only caller passed the value `plan` already carried. **Not
   `_`-prefixed** — an unused parameter with a leading underscore is a working
   signature nobody has to reconcile.
2. `tests/api_support/mod.rs` now carries
   `#![allow(clippy::unwrap_used, clippy::expect_used)]`. This is one of the
   **two** correct shapes; the alternative is a consumer root. It is what
   clears the `xtask` gate violation on `tests/api_coordinator_calls.rs`.

### The two findings from `CliLeadL3` that are settled, and are not to be re-derived

- **Run 1 executed nothing.** exit 101, 0 `Running`, 0 `test result`,
  12 compile errors — **bounded by compile failure of 2 targets**.
  `--no-fail-fast` governs the RUNNER, not compilation.
- **The 21-vs-23 was a rendering artefact** of parallel job interleaving, not
  two missing diagnostics. The count was never 23.
- **rustc's PRIMARY help is more often the diagnosis.** `CliLeadL3` applied the
  secondary `move`, it did not work, the named lifetime was the repair.
***

## 1. The unlock, five passes, each bounded by a different mechanism

| pass | figure | bounded by | what it revealed |
|---|---|---|---|
| 1 | 1 | **parse** | `update/rollout.rs` declared `ReplaceError` twice; :43..EOF nested inside the first |
| 2 | 15 + 1 warn | **lib failure** | L5 `api` 12, L4 `update` 4; L2/L3/L6 clean |
| 3 | 15, all different | **lib failure** | L4 4, L2 3, L5 8 — the layer *under* pass 2 |
| 4 | lib clean | test targets reached | first look at the 38 test binaries |
| 5 | **21 rendered / 23 counted** | **reached the test targets** | 10 failing test targets, see §2 |

**Pass 5's delta is stated, not rounded: the log's `-->` citations sum to 21 and
rustc's per-target `due to N previous error` summaries sum to 23.** One
diagnostic each in `push_fleet_rollback` and `push_keeper_admission` is counted
and not rendered. Pass 5 is therefore *not* a total either.

**A bigger number is not a better measurement.** Pass 1's floor of 1 masked the
entire class behind it, because a parse error is found before name resolution.

## 2. The ten failing test targets at `8e3dc823`, all since fixed

`services_definition_text` 10 · `update_keeper_gate` 2 · `update_recovery` 1 ·
`deploy_remote_identity` 2 · `quickstart_dry_run` 2 · `push_keeper_admission` 2 ·
`api_coordinator_calls` 1 · `push_fleet_plan` 1 · `dev_fan_out` 1 ·
`push_fleet_rollback` 1.

Three classes worth not rediscovering:
- **`render_definition` returns `ProtocolResult<String>`**, so every test that
  formats its result needs an `expect` — and the assertion must stay on the
  rendered **bytes**, not the spec. A test that stops rendering passes whether
  or not the definition writer works.
- **`Permissions::from_mode` is a trait method** on `std::os::unix::fs::PermissionsExt`;
  the trait must be in scope.
- **`AgentPromptWaitOutcome::AGENT_PROMPT_WAIT_OUTCOME_UNSPECIFIED` exists in
  both arms** — the product (`wait_outcome_name`) and the fixture. The product
  *refuses* it, so the fixture renders it to a name no real outcome collides
  with rather than to `"timed_out"`.

## 3. Verified in the tree, not assumed from a note

- **`InventoryError` is already split** — 3 variants, `ColumnDecode { column, cause }`,
  `optional_text` returns `Result`, `collect.rs:227` has its own arm, and no
  `.ok().flatten()` remains in `inventory.rs`. The two remaining `.ok().flatten()`
  are unrelated: a tokio `Lines` stream in `deploy/txn_session.rs:57` and a URL
  parse in `quickstart/add_machine.rs:235`.
- **`ServiceSpec::with_setting` is `pub(crate)`**, two callers in
  `push/coordinator.rs:150,151`. The "third door" is closed by visibility.
- **`add-machine` prints no prose describing `join.sh`** — the whole print path is
  `add_machine.rs:140-148`; a crate-wide grep for `install Bun|clone the
  repo|main\.ts` over `src/` and `tests/` returns zero hits. Not a search artefact.
- **`agent_projection::fields` is a RELOCATION** into `roost_protocol::proto_adapters`
  (integrator's crate), not a dead export. Do not queue it as a `pub`→`fn` candidate.
- **The `Command` enum's 25 variants all have their own doc comment.** Checked by
  reading, because a variant whose doc comment migrated to its neighbour compiles
  perfectly. `CliLeadL` orphaned `KeeperRefresh` then `Api` on consecutive inserts.
- **`src/lib.rs` and `crates/roost-cli/Cargo.toml` are `CliLeadL`'s and settled.**
  If something there does not compile, the likeliest cause is a slice's module.
  Tell them rather than editing.

## 4. The subcommand count is 25, and each of the three numbers is about a different object

Measured from the tree: the `Command` enum has **25** variants and both `name()`
and `dispatch()` answer all 25. **21 is v2's operator-and-daemon surface** (v2's 23
command keys − the 2 v3 drops). v3 adds the four hidden `__remote-*` a deploy runs
over ssh. Asserting 21 drops exactly the four commands the test exists to protect.

The 25: `coord worker keeper status doctor version logs deploy keeper-refresh api
quickstart push join add-machine dev self-link __remote-facts __remote-evidence
__remote-transaction __remote-apply state reset skill test __keeper-contract`.

Only five need an argument to parse: `keeper` (socket), `logs` (app), `deploy`
(host), `keeper-refresh` (host), `add-machine` (`--platform`), plus
`__remote-transaction` (`--kind`). The rest parse bare.

## 5. The allow sweep — MEASURED by property at `7bb4b0e4`, no longer a prediction

**37 files carry `#![allow(clippy::unwrap_used, clippy::expect_used)]`**
(3 under `src/`, 34 under `tests/`) — **not the 36.** The 37th is
`tests/api_support/mod.rs`, which this lead's own `7bb4b0e4` added. A figure
carried across a commit boundary is stale the moment the commit lands; count
it yourself, every time.

The method is **not** a regex and **not** a per-file judgement. It is
`xtask/src/fixture_allow.rs::classify_sites` re-implemented line for line:
brace-depth segment walking, so a site is classified by the scope open **at
its position**, and `.unwrap()` / `.expect(` / `unwrap_err(` / `expect_err(`
matched with the parenthesis required — `unwrap_or` is total and is not a
site. (A regex matching `\.unwrap` without `(` produced 88 sites across 48
correct files on this track. That figure is void.)

**10 DEAD — zero sites outside a `#[test]` body:**
`src/doctor/digest.rs`, `src/doctor/window.rs`, `src/overlay_env.rs`,
`tests/agent_skill_document.rs`, `tests/deploy_remote_identity.rs`,
`tests/doctor_digest_shape.rs`, `tests/push_fleet_plan.rs`,
`tests/push_keeper_admission.rs`, `tests/status_output_shape.rs`,
`tests/update_release_decision.rs`.

**27 LOAD-BEARING — at least one site outside a `#[test]` body.** The largest
are `tests/deploy_machine_transaction.rs` (22), `tests/coord_inventory_query.rs`
(15), `tests/deploy_coordinator_release.rs` (14).

**The four-file disagreement is RESOLVED, by the property and not by picking a
side.** Two classifiers named the same four files and disagreed on all four:
one said `src/doctor/digest.rs` (1 site) and `tests/status_output_shape.rs`
(2 sites) were dead; the other said `tests/deploy_installed_release.rs` and
`tests/deploy_release_path.rs` were. The classifier above gives
**helper sites 1, 2, 6 and 6** respectively:

| file | helper sites | verdict |
|---|---|---|
| `src/doctor/digest.rs` | 0 | DEAD — its one site is inside a `#[test]` |
| `tests/status_output_shape.rs` | 0 | DEAD — both sites are in one `#[test]` body |
| `tests/deploy_installed_release.rs` | 2 | LOAD-BEARING — an ordinary `fn tempdir` at `:213` |
| `tests/deploy_release_path.rs` | 6 | LOAD-BEARING — `fn tempdir` at `:253` plus test bodies |

So both files the other classification called dead have an ordinary `tempdir`
helper carrying sites, and the two files this one called dead have none. **A
disagreement between two independent classifications is a measurement that
localises where the method is uncertain — and here the method was not
uncertain, only the two readings of it.** It is settled.

**What is still a prediction, and only this:** whether `clippy.toml`'s
`allow-unwrap-in-tests` / `allow-expect-in-tests` are *spelled* correctly. A
wrongly-spelled key is silently ignored, so "the keys are present" is not
evidence that the ten DEAD declarations are redundant — the ten are redundant
only if those two keys actually take effect. **That is what the unmodified
clippy run settles, and it is why the sweep is not decided here.**

## 5a. `self-link` vs the `1118cd80` contract — the three deviations, located

`1118cd80` is **not a v2 commit** — it is `docs: roost self-link is new product
surface, so it gets a specification`, and the spec is
`docs/phase6-cli-contract.md` in **this** tree. So "deviates from `1118cd80`"
means "deviates from a document this worktree carries", and the authority is
readable here rather than in a v2 checkout.

All three deviations verified against `src/quickstart/self_link.rs`:

| # | contract | tree | site |
|---|---|---|---|
| 1 | "The old target is printed by name to **stderr before repointing**" | goes to **stdout, after** the `rename` — `write_link` only *returns* `Repaired { previous }` and `run()` prints it afterwards | `self_link.rs:93-95`, `:199` |
| 2 | "The outcome on stdout as **one word** — `created`, `repaired`, or `unchanged`" | a sentence: `~/.local/bin/roost -> /path (already correct)`; and the variant is *named* `AlreadyCorrect`, so even the word is wrong | `LinkOutcome::sentence`, `:63-78`, printed at `:95` |
| 3 | a **directory** is "refused, named. Never `remove_dir_all`" | one `Ok(metadata) if !is_symlink()` arm serves **both** a regular file and a directory, and names `rm {0}` — which **fails on a directory** | `self_link.rs:170-176` |

**#1 is the Phase-7-critical one and it is an ordering property, not a text
one.** The cutover runs this unattended; if the `rename` fails, the old target
must already be in the transcript. Printing it afterwards loses it exactly when
it matters. The test that exists today
(`a_link_that_still_points_at_an_older_install_is_repointed_and_the_old_target_is_said`)
asserts the sentence *contains* the old path — it **cannot see the ordering**,
because `sentence()` is called after `write_link` has already returned.

**The fix shape is a pre-rename hook on `write_link`, not a plan/apply split.**
Splitting the classification from the write opens a TOCTOU: a path that was a
symlink at plan time and is a regular file at apply time would be `rename`d
over — **clobbering the operator's own file**, which is the one data loss this
command exists to prevent. Check and write must stay in one function.
***

## 6. NOT DONE — do not assume them

1. **The first `cargo test -p roost-cli --no-fail-fast` in this project's history**
   was pending at the time of writing. No pass count exists. Two agreeing runs
   are the gate.
2. **No clippy has ever been run on `roost-cli`** — by any lead. An absence is
   not a floor of zero. When it runs: `exit 0` after reaching the end of the
   target list is a **TOTAL**; a non-zero exit's diagnostic count is a **floor**
   bounded by where it stopped.
3. **`self-link` still deviates from the `1118cd80` contract in its output** — the
   old target goes to stdout *after* the rename rather than stderr *before* it
   (the Phase-7-critical one), stdout is a sentence rather than one of
   `created`/`repaired`/`unchanged`, and the directory refusal names `rm`, which
   will not work on a directory. Behaviour is correct; the I/O is not.
4. **`cargo fmt`, `cargo xtask lint`, the `v3-coord` merge (row M2), and the 11
   mutation rows (L2-1..3, L3-1..8, L-1, L-2) have not run.** Two of the three
   recorded wrong-target corrections now point into code this wave changed
   (`write_link`, and `rollout.rs` line numbers) — **re-grep every anchor before
   running a row.** A row whose anchor moved mutates nothing, and its silence
   reads as "does not bite."
5. **Fresh-host `deploy` is blocked** on Track W's W-2/L4
   (`ENV_BOOTSTRAP_TOKEN` redemption, `runtime/bootstrap_redeem.rs`), owner
   `agent://WorkerLeadW3`. CHOSEN: read at the point of use, not stored in
   `WorkerBoot`.

## Build

```
export PATH="$HOME/.cargo/bin:$PATH" CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=2 \
  CARGO_TARGET_DIR=/home/almalinux/repos/roost-v3-cli/target-track
```

One compiler per target dir, and the lead runs it. `CliLeadL` and `CliLeadL2` are
stood down and run no cargo here. The only sanctioned disk reclaim is
`cargo clean` on this directory; nothing else.

All 158 files are under the 400-line cap (largest `services/deploy_transaction.rs`,
396). One `macro_rules!` removed.

## Scratch dry-run environment (NOT in the worktree)

Target `almalinux@localhost` over real sshd; `~/.ssh/config` has a `Host localhost`
stanza pointing at `~/.ssh/id_ed25519_roost_deploy` (`roost deploy` passes no `-i`).
**`~/.ssh/authorized_keys` gained that key's `.pub` line**; backup
`~/.ssh/authorized_keys.roostv3-backup`; revert with
`cp ~/.ssh/authorized_keys.roostv3-backup ~/.ssh/authorized_keys`. Driver
`/tmp/roost-dryrun.sh`. Isolation by environment: `ROOST_COORDINATOR_BIND`,
`ROOST_COORDINATOR_DB`, `ROOST_COORD_DATA_DIR`, `ROOST_COORD_LOG_DIR`,
`ROOST_SERVICE_DIR`, `ROOST_WORKER_DATA_DIR`, `ROOST_WORKER_LOG_DIR`.

## 7. The 11 mutation rows — anchors re-grepped against this tree

**All 12 anchors were re-grepped, not read off the row table.** Ten are exact and
unmoved. Two had drifted, and one of those drifted in a way that matters.

| row | anchor as re-grepped | state |
|---|---|---|
| L2-1 | `push/plan.rs:211` `if resolved.ambiguous {` | **exact** |
| L2-2 | `push/rollout.rs:230` `if finalizing {` | **exact** |
| L2-3 | `push/admission.rs:76` | **RE-POINTED, see below** |
| L3-1 | `quickstart/endpoint.rs:83` `pub fn coordinator_settings` | **exact** |
| L3-2 | `quickstart/plan.rs:158` `print_plan`; `definition_path` at :189/:196/:230 | **exact** |
| L3-3 | `quickstart/add_machine.rs:227-235` `dial_url` — installed wins via `.or_else` | **exact** |
| L3-4 | `quickstart/add_machine.rs:77` `EnrollmentPlatform::from_name` | **exact** |
| L3-5 | `quickstart/join.rs:144` `joined_build_sha`, dirty branch at :146 | **exact** |
| L3-6 | `quickstart/grant.rs:72` `PLACEHOLDER_BEARER` | **exact** |
| L3-7 | `quickstart/self_link.rs:136` `write_link` | **exact** |
| L3-8 | `quickstart/self_link.rs` `write_link`, `read_link` equality branch at :157-164 | **exact** |
| L-1 | `deploy/identity_env.rs:168` `worker_install_environment` | **runnable, and now proves more** |
| L-2 | `services/service_spec.rs:237` `with_decided_one_shots` | **exact** |

### L2-3 — the row names the wrong function

The row reads "change the first argument to `classify_fleet_keeper_updates`".
**Its first parameter is `targets: &[FleetRolloutTarget]`.** The comparison the
row is about is one level down, at **`admission.rs:75-78`**:

```rust
let Some(admission) = keeper_update_admission(
    target_contract,                  // <- :76, the RELEASE's contract
    worker.keeper_runtime.as_ref(),   // <- :77, the RUNNING keeper's
    &open_sessions,
```

The recorded mutation text — swap that first argument for
`&worker.keeper_runtime.as_ref().unwrap().running_contract` — is **still exactly
right**; only the function it was attributed to is wrong. That makes the keeper
compare against itself, which is the property's whole content.

**One thing that makes this row dangerous to run by search.** The same file
legitimately contains a keeper-against-itself comparison, at
`rollback_keeper_update:127-128`, where `source_contract` and `target_contract`
are both `running.running_contract.clone()`. **That one is correct** — its own doc
comment says why: a rollback ships no new keeper, it restores the one the machine
already had. A mutation aimed at "the keeper compared against itself" by pattern
would hit the one correct instance and leave the one under test untouched. Point
this row at `:76` by line, never by pattern.

### L-1 — the anchor is fine, and its control now proves more than recorded

`is_one_shot_authorization` no longer appears in `identity_env.rs`; the strip is
now an explicit loop at the top of the function:

```rust
for key in [ENV_BOOTSTRAP_TOKEN, KEEPER_FORCE_LIVE_RETIRE_ENV] {
    values.remove(key);
}
```

The row's mutation is "re-insert `values.retain(|key, _| !is_one_shot_authorization(key));`
at the end of `worker_install_environment`". **That still compiles and still bites** —
`is_one_shot_authorization` is live at `services/service_environment.rs:63`.

But the control got stronger for free. The row's must-still-pass test is
`a_deploy_never_carries_a_one_shot_grant_forward` (a PRIOR install's grant). Under
the mutation that test **still passes**, because the top-of-function loop is
untouched — it is the *override* path the mutation breaks. So the row now
separates two rules that used to be one line: the resolve-side strip and the
arming site. That asymmetry is the point of the control, and it is now real
rather than asserted.
