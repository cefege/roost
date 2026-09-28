# WAgentsInstall — phase-2 handoff (self-sufficient)

Slice: W-AGENTS integration install. Ports v2 `apps/worker/src/agents/{install-integrations,integration-assets,
integration-assets.generated,integration-install-proof,integration-install-transaction,standalone-integration}.ts`
and embeds the agent-side TS plugins `integrations/{omp,pi}/*.ts` + `report-transport.ts` in the crate (they run
inside OMP/Pi, stay TypeScript, and must survive the Stage-6 deletion of `apps/`). v2 boot call site:
`apps/worker/src/main.ts:139-143` (`await installAgentIntegrations()` in try/catch → `log.warn("agent-status",
"integration_install_failed")`), after `runInstall` (enrollment) and before `startLocalTerminalDoor`.

Drafts root: `target-track/drafts/WAgentsInstall/` mirrors repo paths. Phase 1 compiled NOTHING (rules forbade
cargo); every .rs was `rustfmt --edition 2024`-clean (toolchain 1.98.1) at drafting time. Expect small compile
fixes. All 4 asset copies were `cmp`-identical to v2 at drafting time (re-run the cmp below before moving).

## 1. Files to move into place (cp -r drafts/WAgentsInstall/crates → repo crates/)

| file | lines | purpose |
|---|---|---|
| crates/roost-worker/assets/integrations/omp/roost-agent-state.ts | 218 | verbatim v2 copy (`include_str!`) |
| crates/roost-worker/assets/integrations/omp/roost-agent-reference.ts | 100 | verbatim v2 copy |
| crates/roost-worker/assets/integrations/pi/roost-agent-state.ts | 96 | verbatim v2 copy |
| crates/roost-worker/assets/report-transport.ts | 189 | verbatim v2 `agents/report-transport.ts`; at `assets/` (not `assets/integrations/`) so the sources' verbatim `../../report-transport.ts` import still resolves |
| src/agents/integration_assets.rs | 182 | ids (`AgentIntegrationAssetId` omp-status/omp-reference/pi-status), `AgentIntegrationRuntime` {Omp,Pi}, `PerRuntime<T>{omp,pi}.get()`, `AGENT_INTEGRATION_ASSET_SPECS` (3, with `source: include_str!`), `RETIRED_AGENT_INTEGRATION_SPECS` (omp `roost-omp-session-api.ts`), private `REPORT_TRANSPORT_SOURCE`, `load_agent_integration_assets()` (compose + ownership assert), private `compose_standalone_integration` (v2 regex `(?m)^import\s*\{[^}]+\}\s*from\s*"[^"\r\n]*report-transport\.ts";$`, first match, `NoExpand`) |
| src/agents/install_proof.rs | 342 | `IntegrationInstallError {Refused(String), Io{operation,path,source}}` (+`refused()`, `io(op,&Path)` map_err mapper, `is_not_found()`), snapshots/plans, `integration_path_comparison_key` (NFC; lowercase on MacOs/Windows), `has_integration_ownership`, `preflight_integration_directory`, `inspect_integration_target`, `assert_integration_target_unchanged`, `inspect_integration_directory`, `assert_integration_directory_snapshot`, `integration_lstat_if_present`, `same_integration_identity`, `normalize_lexically` (node `path.join` normalisation), private `canonicalize_planned_path` |
| src/agents/install_stage.rs | 226 | pub(crate): `StagedAsset`, `PreparedDirectory` (+`assert_stable`), `prepare_directory`, `stage_asset` (create_new 0600 + fsync + dir fsync), `cleanup_stages`, `cleanup_created_directories`, `sync_directory`; private `create_stage_directory` (`.roost-integration-stage-<16hex>` via `std::hash::RandomState`, mode 0700, 64 attempts) |
| src/agents/install_mutation.rs | 278 | pub(crate): `AssetMutation`/`RetirementMutation` (`new`, `apply`), `InstallMutation` enum, `rollback_mutations` |
| src/agents/install_transaction.rs | 252 | pub: `COLLIDING_DIRECTORIES_REFUSAL`, `IntegrationAssetInstallPlan`, `IntegrationRetirementPlan` (+`target_plan()`), `InstallHookResult`, `IntegrationInstallTestHooks {before_final_validation, after_committed_mutation}` (manual Debug), `commit_integration_install(&PerRuntime<IntegrationDirectoryPlan>, &[assets], &[retirements], HostPlatform, &mut hooks)` |
| src/agents/install_integrations.rs | 298 | pub: `PI_CODING_AGENT_DIR_ENV`, `PI_CONFIG_DIR_ENV`, report types, `resolve_pi_extension_dir`/`resolve_omp_extension_dir(&dyn EnvSource, &Path)`, `install_agent_integrations(env, home, platform)`, `_install_agent_integrations_for_test(.., &mut hooks)`, `install_agent_integrations_at_boot(platform)` (async; `spawn_blocking`; logs `integration_install_failed` and returns on any failure; info line with installed ids) |
| tests/integration_install_support/mod.rs | 68 | shared fixture (`#![allow(dead_code)]`), re-exports `Scratch` from `../credential_support/scratch.rs` via `#[path]`; `omp_dir`, `pi_dir`, `entries`, `is_absent`, `installed_ids`, `installed_path` |
| tests/agent_status_installer.rs | 273 | 10 tests (see §4) |
| tests/agent_status_install_rollback.rs | 133 | 3 tests (see §4) |
| tests/agent_status_integration_ownership.rs | 97 | 3 tests (see §4) |

Verify copies first: `for f in integrations/omp/roost-agent-state.ts integrations/omp/roost-agent-reference.ts
integrations/pi/roost-agent-state.ts; do cmp crates/roost-worker/assets/$f apps/worker/src/agents/$f; done;
cmp crates/roost-worker/assets/report-transport.ts apps/worker/src/agents/report-transport.ts`.

## 2. Exact edits to existing files (re-read each right before editing; siblings edit concurrently)

1. `crates/roost-worker/src/agents/mod.rs` (78 lines at drafting; NO `mod` lines yet — siblings WAgentsReport/
   WAgentsDetect/WAgentsPrompt add theirs too). Insert after the `//!` header block (i.e. before the
   `/// How many distinct built-in agents this worker can recognise.` doc of `pub enum BuiltinAgentId`), merged
   alphabetically with siblings' lines:
   ```rust
   pub mod install_integrations;
   mod install_mutation;
   pub mod install_proof;
   mod install_stage;
   pub mod install_transaction;
   pub mod integration_assets;
   ```
   (`install_mutation`/`install_stage` are private: only pub(crate) items, reached from sibling modules.)
2. Root `Cargo.toml` `[workspace.dependencies]`: after the line `regex = "1.13.1"` add
   `unicode-normalization = "0.1.25"` (already in Cargo.lock via `stringprep`; source present in
   ~/.cargo/registry). Reason: v2 collision key is `path.normalize("NFC")` (+ lowercase on darwin/win32).
3. `crates/roost-worker/Cargo.toml` `[dependencies]`: after `roost-term.workspace = true` add
   `unicode-normalization.workspace = true`. Cargo.lock gains the roost-worker edge on the next cargo run
   (cross-owner edits: report both Cargo.toml files + Cargo.lock).
4. `crates/roost-worker/src/runtime/boot_sequence.rs` (386 lines at drafting; cap 400). Anchor: the statement
   `let platform = supported_host_platform()` … `.map_err(...)?;` (≈line 111-112) immediately followed by
   `let door = LocalDoor::bind(Some(&door_bind())).await?;`. Insert between them:
   ```rust
       // v2 main.ts:139: integrations are current before the door or link can start an agent.
       crate::agents::install_integrations::install_agent_integrations_at_boot(platform).await;
   ```
   If that pushes the file over 400, drop the comment line. `runtime/owners.rs` is NOT a fallback: at drafting
   `WorkerOwners::build(stack, uplink, process_epoch, pool) -> Self` was sync with no `platform`, so it cannot
   `.await` this step. If boot_sequence has no room even for the one line, ask the lead. Never inline more than
   the one call.
No link_drain arm and no downstream kind are involved; nothing replaces an absent-owner arm.

## 3. Build / test commands (phase 2)
- compile: `/tmp/wcheck.sh 'agents/(install_|integration_assets)|agent_status_install|integration_install_support|boot_sequence'` (timeout 3600)
- tests: `/tmp/wcargo.sh test -p roost-worker --test agent_status_installer --test agent_status_install_rollback --test agent_status_integration_ownership`
- clippy: `/tmp/wcargo.sh clippy -p roost-worker --all-targets -- -D warnings`
- `rustfmt +1.98.1 --edition 2024 --check <my files>` (NOT cargo fmt). Every file ≤400 lines.

## 4. v2 tests → Rust tests (all ported; names)
v2 `apps/worker/tests/agents/agent-status-installer.test.ts` →
- `tests/agent_status_installer.rs`: `resolves_default_and_configured_pi_and_omp_directories` (+ precedence:
  PI_CODING_AGENT_DIR beats PI_CONFIG_DIR for OMP), `installs_the_complete_typed_assets_byte_for_byte_and_is_idempotent`
  (mode 0600, same inode on 2nd pass, loader dirs list exactly the installed files — no stage/tmp leftovers),
  `every_asset_installs_as_a_standalone_module_with_the_transport_spliced_in` (new guard: no
  `report-transport.ts";` import left, `import net from "node:net";` present), `removes_an_owned_retired_omp_asset_only_after_successful_preflight`,
  `preserves_an_unowned_file_at_the_retired_filename`, `rejects_a_direct_omp_and_pi_destination_collision_without_mutation`,
  `rejects_a_symlink_directory_alias_without_installing_either_runtime`, `refuses_a_user_owned_destination_and_installs_the_other_runtime`,
  `refuses_an_owned_filename_symlink_and_installs_the_remaining_assets`,
  `rejects_absent_case_only_runtime_aliases_on_darwin_and_windows_only` (+ Linux `.PI`/`.pi` install as distinct).
- `tests/agent_status_install_rollback.rs`: `preserves_an_unowned_target_that_appears_after_staging`
  (before_final_validation hook), `rejects_a_loader_symlink_swap_at_the_final_mutation_boundary`,
  `rolls_back_owned_replacements_and_absent_creates_after_a_commit_failure` (after_committed_mutation fails at 2;
  prior owned file restored with the SAME inode).
v2 `agent-status-integration-ownership.test.ts` → `tests/agent_status_integration_ownership.rs`:
`the_marker_is_recognized_in_any_comment_line_and_nowhere_else` (marker on line 106),
`adopts_and_overwrites_an_installed_asset_marked_below_the_file_head`, `the_commit_guard_passes_a_target_marked_below_the_file_head`.
NOT ported (TS code that stays TS, runs under bun): `agent-status-integrations.test.ts`,
`agent-reference-report-transport.test.ts` — they exercise the plugins themselves; they lose their home when
`apps/` is deleted (open gap for the lead).

## 5. Planned mutations (each must make a test fail; revert after)
1. install_proof.rs `has_integration_ownership`: `content.split('\n').any(` → `content.split('\n').take(8).any(` → ownership marker/adopt tests fail.
2. install_integrations.rs asset planning `Err(error) => failed.push(refused_target(..))` → `Err(error) => return Err(error)` → user-owned + symlink-target tests fail.
3. install_mutation.rs rollback `durable_remove(target)?;` → `{}` → rollback test fails (reference file remains / hard_link EEXIST).
4. install_transaction.rs `is_current()` body → `false` → idempotence test fails (inode changes).
5. install_proof.rs `assert_integration_directory_snapshot` condition → `if false` → loader-swap test fails (files land in `omp-first`).
6. install_proof.rs `assert_integration_target_unchanged` `current.as_ref() != plan.existing` → `false` → raced-target test fails (message becomes "ownership changed").
7. install_proof.rs comparison key: MacOs arm → no lowercase → case-alias test fails.
8. install_integrations.rs `plan_retirement` `remove` → `existing.is_some()` → unowned-retired test fails.
9. integration_assets.rs compose → return `integration_source.to_owned()` → standalone-module test fails.

## 6. Interfaces agreed with siblings
- WAgentsReport (agent://WorkerLead2W2.WAgentsReport) agreed: THIS slice owns the only `include_str!` of
  `assets/report-transport.ts` and the port of `standalone-integration.ts` (in integration_assets.rs). They will NOT
  create `agents/report_transport.rs` / `agents/standalone_integration.rs`. Their slice: report_server/report_protocol/
  environment/manifest_engine/manifests (+ host/local_endpoint.rs). v2-map rows for report-transport.ts and
  standalone-integration.ts point at `agents/integration_assets.rs`.
- No other sibling shares code with this slice.

## 7. Decisions / deviations (for the report)
- Filesystem ops are sync std::fs (v2 async fs/promises); the boot step runs them on `spawn_blocking` and awaits,
  as v2 awaits. Hooks are sync `Box<dyn FnMut>`.
- Durability is POSIX-only (Windows unsupported in v3): staged file create_new+0600+fsync+stage-dir fsync (v2 wrote
  a `.tmp-*` then renamed inside the private stage dir — invisible difference); `durableRemove` = unlink + parent fsync.
  Like v2, no fsync of the loader dir after link/rename.
- `same_integration_file_snapshot` is derived `PartialEq` on `Option<&IntegrationFileSnapshot>` (content+dev+ino).
- File content is read lossy-UTF-8 (node `readFile(utf8)` semantics).
- Missing HOME at boot → warn + skip (v2 os.homedir() would fall back to passwd; roost-host refuses missing HOME).
- `source_path` of the v2 catalog not ported (no reader; include_str! embeds the path).
- FAILURE-INDEX entry "### Roost cannot upgrade the integration asset Roost installed": Guard should be rewritten
  to `crates/roost-worker/tests/agent_status_integration_ownership.rs` + `tests/agent_status_installer.rs`
  (`refuses_a_user_owned_destination…`, `refuses_an_owned_filename_symlink…`). Lead edits docs.

## 8. Consumers of new values
`AgentIntegrationInstallReport.installed` → boot info line in `install_agent_integrations_at_boot`; `.failed` → warn per
target inside the pass + count in the boot line; `IntegrationInstallError::is_not_found` → `preflight_integration_directory`;
`IntegrationTargetPlan.remove` → `assert_integration_target_unchanged`; `PreparedDirectory.created` →
`cleanup_created_directories`; `IntegrationInstallTestHooks` → `commit_integration_install` (tests only set them).

## 9. Open questions
- boot_sequence line budget (see §2.4 fallback).
- TS plugin tests (§4 NOT ported) need a home after Stage 6 (bun test over `crates/roost-worker/assets`?) — lead decision.
