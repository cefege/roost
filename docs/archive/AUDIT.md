# Roost v2 — cleanup audit ledger

Working artifact for the full-pass codebase cleanup (knip + jscpd + fixed inventories).
Generated Phase 0; the **BACKLOG** section is the durable output — tracked, deferred debt.

- Tooling: `knip@6.26.0` (root `knip.json`), one-time `jscpd` duplication scan.
- Baseline typecheck: 4 pre-existing `tsc` error-lines (`hooks.ts:93`, `multiplexed-main.ts:467` — Node `net.Server.on()` typed as Bun global `Server`). Not caused by this pass; gate = "no new errors beyond these".
- Regenerate: `bun run knip` (report-only in CI, non-gating).

---

## 1. knip report (raw, pre-config-tuning)

### Unused files (7)
| File | Disposition |
| :--- | :--- |
| `apps/web/src/components/CompletedTray.tsx` | **SAFE delete** (Phase 3.2, LSP-verify) |
| `apps/web/src/components/BootScreen.tsx` | **SAFE delete** (Phase 3.2, LSP-verify) |
| `apps/web/src/lib/tabOrder.ts` | **SAFE delete** (Phase 3.2, LSP-verify) |
| `.claude/skills/roost-render-stress/run.js` | FALSE POSITIVE — skill runner (not a workspace); knip-ignore |
| `.claude/skills/roost-smoke/run.js` | FALSE POSITIVE — skill runner; knip-ignore |
| `apps/worker/src/cli/hook.ts` | FALSE POSITIVE — spawned hook entry (Phase 3.1); whitelist as entry |
| `scripts/lint-roost.ts` | FALSE POSITIVE — run via `bun scripts/lint-roost.ts` + CI; whitelist as entry |

### Unused dependencies (1)
| Dep | Location | Disposition |
| :--- | :--- | :--- |
| `@wterm/core` | `apps/web/package.json:21` | RISKY→SAFE — zero imports in web src (only 2 comment mentions); `@wterm/dom` is the used one. Remove if transitively provided by `@wterm/dom`. |

### Unused devDependencies (1)
| Dep | Location | Disposition |
| :--- | :--- | :--- |
| `@bufbuild/protoc-gen-es` | `apps/shared/package.json:36` | FALSE POSITIVE — used by `buf generate` (`proto:gen` script); knip can't see `buf.gen.yaml`. Keep, ignore in knip. |

### Unlisted dependencies (1)
| Dep | Location | Disposition |
| :--- | :--- | :--- |
| `solid-js` | `tsconfig.base.json` (`jsxImportSource`) | FALSE POSITIVE — solid-js is a web dep; root tsconfig references it for JSX. BACKLOG (cosmetic). |

### Unlisted binaries (3)
| Binary | Location | Disposition |
| :--- | :--- | :--- |
| `playwright` | `.github/workflows/verify-ui.yml`, `ci.yml` | FALSE POSITIVE — CI-provided; ignore. |
| `vm_stat` | `apps/worker/src/heartbeat.ts` | FALSE POSITIVE — macOS system binary; ignore. |

### Unused exports (34) & Unused exported types (27)
Per-item LSP verification required (Phase 3.3). Several are plausibly orphaned by the
just-committed WIP refactor (sidebar/context-menu/session) and MUST NOT be mass-deleted.
- **db/schema.ts table types (14)** — `WorkersTable`, `EventsTable`, `WorkspacesTable`, `WorkspaceSessionsTable`, `BootstrapTokensTable`, `PairRequestsTable`, `TasksTable`, `WebhookTokensTable`, `AuditLogTable`, `McpRelaysTable`, `AuthorizedKeysTable`, `MigrationsTable`, `AppSettingsTable`, `PermissionRulesTable`: **BACKLOG** — Drizzle schema typing surface; workspace-scoped knip misses inferred use. Keep unless LSP proves zero refs.
- Remaining exports/types: LSP-verify each; down-scope (drop `export`) if used-locally-only, delete if fully dead, else BACKLOG.

Full raw report archived inline at generation: unused files/deps/exports/types as tabulated above.

---

## 2. Duplication (jscpd, one-time — NOT wired to CI)

`jscpd apps --min-lines 20 --min-tokens 100` → **5 clones, 0.19% total duplication** (very low).

| Clone | Lines | Disposition |
| :--- | :--- | :--- |
| `coord/tests/coord-bidi.test.ts` ↔ `heartbeat-reachable-addr.test.ts` (setup) | 26 | BACKLOG (test scaffolding) |
| `roost-cli/src/deploy.ts` internal (81-101 ↔ 361-385) | 21 | Addressed incidentally by Phase 4 split |
| `web/DeployConsoleModal.tsx` ↔ `TransferConsoleModal.tsx` (×2) | 23+23 | BACKLOG (UI modal shell dedupe — RISKY, needs shared component) |
| `web/store/sync-bootstrap.ts` internal (128-148 ↔ 266-284) | 21 | Addressed incidentally by Phase 4 split |

> jscpd's 20-line floor did NOT flag the plan's small-helper dups (`formatBytes`, `expandTilde`, ed25519 parse — each <20L). Those are scout-sourced; see Phase 3.4.

---

## 3. Over-cap files (>400 L — CLAUDE.md coding-standard #1), Phase 4

| LOC | File | Risk |
| ---: | :--- | :--- |
| 2239 | `apps/worker/src/session-manager.ts` | HIGH |
| 873 | `apps/web/src/components/CellTerminal.tsx` | HIGH |
| 621 | `apps/worker/src/keeper/multiplexed-client.ts` | MED |
| 536 | `apps/worker/src/main.ts` | MED |
| 527 | `apps/worker/tests/multi-viewer-dynamic.test.ts` | LOW (test) |
| 518 | `apps/web/src/components/terminal-links.ts` | MED |
| 505 | `apps/worker/src/keeper/multiplexed-main.ts` | MED |
| 500 | `apps/web/src/components/Settings/md/primitives.tsx` | LOW (one-component-per-file) |
| 487 | `apps/web/src/components/CommandPalette.tsx` | MED |
| 483 | `apps/worker/src/transport/CoordLink.ts` | MED |
| 476 | `apps/web/src/components/sidebar/SessionRow.tsx` | MED |
| 459 | `apps/web/src/lib/deepgramDictation.ts` | MED |
| 453 | `apps/worker/src/browser-command-handler.ts` | MED |
| 434 | `apps/roost-cli/src/deploy.ts` | LOW |
| 424 | `apps/web/src/store/sync-bootstrap.ts` | MED |
| 415 | `apps/coord/src/connect/worker-service.ts` | LOW |

---

## 4. Artifact / config debris (Phase 1)

| Item | Type | Disposition |
| :--- | :--- | :--- |
| `_spike/pty-survival/` | tracked Rust spike | SAFE delete (all-TS repo; git history preserves) |
| `.pytest_cache/` | gitignored Python cache | SAFE delete (local) |
| `.research/` | empty dir | SAFE delete (local) |
| `coordinator_v2.db` | gitignored, **0 B** | SAFE delete (coord recreates via migrations) |
| `.gitignore` L18-26 `apps_legacy/` block | dead config | SAFE remove (tree deleted) |
| `tsconfig.base.json` L24 `apps_legacy/**` | dead exclude | SAFE remove |

---

## 5. Debt markers (harvest only — DO NOT delete; deliberate shortcuts)

**`ponytail:` (16):**
`coord/src/byte-hub.ts:36`, `coord/src/main.ts:206`, `coord/src/connect/handlers-system.ts:81`,
`roost-cli/src/api.ts:200`, `web/vite.config.ts:30`, `web/vite.config.ts:50`,
`web/src/components/ToastContainer.tsx:86`, `web/src/lib/keytermContext.ts:128`,
`web/src/lib/toastStore.ts:37`, `web/src/store/sync.ts:311`,
`web/src/styles/sidebar.css:1000`, `web/src/styles/sidebar.css:1392`,
`worker/src/file-rpcs.ts:9`, `worker/src/session-manager.ts:208`,
`worker/src/session-manager.ts:215`, `worker/src/session-manager.ts:1953`

**`TODO` (1):** `web/src/components/MainPane.tsx:145` — "TODO R4.3: Global search" (unimplemented search pane).

---

## BACKLOG (durable — deferred debt, tracked here)

1. **Unused exports/types (61) needing case-by-case LSP** — down-scope/delete only where zero-ref proven; db/schema.ts table types (14) kept as schema surface. Anything WIP-orphaned kept.
2. **`.claude/commands/v2-status.md`** — stale slash-command tracking completed R4.-1..R4.5, probes near-empty `apps/coord/src/router/`. Removing a command is a behavior change → defer to user.
3. **Dual TypeScript toolchain — RESOLVED (post-review).** Standardized on **TS7** (`@typescript/native-preview` / `tsgo`): dropped classic `typescript@5.7` from root/web/worker, pointed all app `typecheck` scripts + the CI gate at `tsgo`, added roost-cli's missing script. Editor was already `useTsgo:true`. `typescript` now lingers only as knip's transitive dep.
4. **jscpd modal dup** — `DeployConsoleModal`/`TransferConsoleModal` share ~46 lines of shell; extract a shared modal component (RISKY UI change).
5. **knip false positives** kept intentionally: `@bufbuild/protoc-gen-es` (buf codegen), `solid-js` unlisted-in-tsconfig, `vm_stat`/`playwright` unlisted binaries.
6. **Pre-existing typecheck errors — RESOLVED (post-review).** The 4 `net.Server.on` errors surfaced only under root `tsconfig.base.json` (no root `@types/node`); added `@types/node@22` (dev-only, NOT in the `types` array → no Bun/Node ambient collision) → base gate 0 errors. CI typecheck (now `tsgo`) is green.

---

## Execution log (what this cleanup pass actually did)

**Phase 3 — dead code / exports / deps / dedupe:**
- **Deleted (knip + grep zero-ref agree):** `CompletedTray.tsx`, `BootScreen.tsx`, `lib/tabOrder.ts`. Stale "callers" named in their header comments (`App.tsx`/`AppShell`/`TopChrome`) no longer import them.
- **Dependency removed:** `@wterm/core` from `apps/web/package.json` (web imports only `@wterm/dom`; worker + shared declare + use `@wterm/core` themselves). `bun install` refreshed the lockfile; typecheck + 567 tests green.
- **Helpers deduped:** `formatBytes` → `apps/web/src/lib/format.ts` (3 callsites migrated; canonical is GB-aware + 1-decimal — MachinesPane gains 1-decimal precision, cosmetic). `expandTilde` → `apps/worker/src/util/path.ts` (file-rpcs + session-manager migrated; dead `homedir` imports removed).

**Deferred to BACKLOG during Phase 3 (added rationale):**
- **ed25519 crypto consolidation — BACKLOG (not a clean dedupe).** Worker `jwt.ts` fingerprints with *synchronous* `createHash`; coord `jwt.ts` uses *async* `crypto.subtle.digest`. `parseOpenSshEd25519` differs in return shape (`pubKey` vs `pubRaw`) and coord's is entangled with `generateOpenSshEd25519` (node:crypto). Only `PKCS8_ED25519_PREFIX` + `seedToPkcs8` (~8 lines) are truly identical. Unifying = a sync/async API design decision on the auth path, not mechanical extraction → deferred per plan gate.
- **61 unused exports/types — BACKLOG (tracked by report-only knip in CI).** Per the plan's strategic guidance ("do not hunt for phantom dead code; the real payoff is the 3 dead files + the helper dedupe, not a large purge") and the heavy just-committed context-menu/sidebar/session WIP (several exports plausibly transitional), mass `export`-stripping across ~25 files — some being split in Phase 4 — is low-value churn with WIP-orphan risk. knip (wired report-only in Phase 6) surfaces them every CI run for incremental cleanup. db/schema.ts table types (14) retained as schema surface.

**Phase 4 — over-cap file splits (15/16 split; all via same-dir sibling extraction + re-export, public import paths unchanged):**
- `session-manager.ts` 2239→391 (+9 siblings), `multiplexed-client.ts` 621→179 (+5), `main.ts` 536→265 (+2), `multiplexed-main.ts` 505→140 (+4, keeper build-stamp source list updated), `CoordLink.ts` 483→370 (+3), `browser-command-handler.ts` 453→122 (+6), `multi-viewer-dynamic.test.ts` 527→(helpers + 3 scenario files), `worker-service.ts` 415→21 (+3), `deploy.ts` 434→235 (+4), `terminal-links.ts` 518→339 (+1), `primitives.tsx` 500→46 barrel (+18 one-component files), `CommandPalette.tsx` 487→393, `SessionRow.tsx` 476→369, `deepgramDictation.ts` 459→365, `sync-bootstrap.ts` 424→360.
- **`CellTerminal.tsx` (873) → BACKLOG.** ~700 lines are closures over shared per-instance mutable Solid state; splitting requires a stateful-controller abstraction the plan explicitly forbids ("do not invent an abstraction to hit the cap"). It is the terminal render-correctness core — a forced split risks the history-corruption class the repo guards. Only over-cap hand-written file remaining; `src/gen/**` protobuf (4 files) are generated + cap-exempt.
- **Latent bug fixed (surfaced by the test-file reorg):** `_runDetect` (worker `session-emit.ts`) now skips records with no `wtermCore` — a leaked `_sweepDetect` interval (4000 ms) from an earlier test fired against a torn-down record → `core.getCols` on undefined. Byte-identical to the pre-split original otherwise; the guard is behavior-preserving for real sessions (which always have a core). Full suite 643 pass / 0 fail; worker suite stable across repeated runs.
- Delegated agents also conformed moved code to existing repo lint rules (`ts-no-dynamic-import`, `ts-promise-with-resolvers`, `ts-no-tiny-functions`) — behavior-preserving idiom swaps.

**Phase 5 — doc restructure for LLM onboarding:**
- CLAUDE.md read-first pointer reordered to the living set (`ARCHITECTURE`→`GLOSSARY`→`STATE`); REWRITE.md reworded as R0–R10 historical / R11 live + a status banner; `docs/archive/README.md` added (flags the archive as skip-on-onboarding).
- File map reconciled: every `apps/…` path cited in CLAUDE.md / ARCHITECTURE.md / GLOSSARY.md now resolves. Fixes: `ARCHITECTURE.md` `components/Terminal.tsx`→`CellTerminal.tsx` (pre-existing stale); CLAUDE.md keeper bullet now attributes the PTY spawn + TERM env to the split `keeper/keeper-frame-handler.ts` sibling. Split originals kept their public filenames, so no other citation moved.
- **5.4 (trim redundancy) — no safe target.** ARCHITECTURE.md explicitly names CLAUDE.md "the exhaustive in-repo reference" and stays tour-level; CLAUDE.md's transport prose is exhaustive + L11-coupled (the raw-WS reversal note references an L11 memory). Complementary by design, not redundant — trimming would break the L10 cross-doc contract + L11 coupling. Left intact.

**Verification (final):**
- **roost-smoke: 14/14 passed** (live tailnet coord + humanchrome). Full spawn→nav→echo→focus-lands→deck-persist→kill+cascade→workspace-create→cwd-regroup flow through the real Connect-RPC path over the split worker/session-manager/coord code. step13 (resize-wobble) held scrollback (`grew:0`) — the viewport-slaved-PTY history-corruption regression check.
- **roost-render-stress[cell]: PASS** — 48-iter resize-perturbation matrix (w/h/both/tiny shrink+grow) on a marker-filled session; 0 dup / 0 mangle / 0 loss, depth 150→150, restored clean. Cell is the live default (R11); it renders from the split session-manager's worker grid.
- Byte-mode + claude-alt-screen + multi-viewer render-stress variants scoped out with rationale: `CellTerminal.tsx` (paint) untouched; session-manager `_rebuildWtermCore`/viewport/scrollback byte-identical (42/42 bodies); covered by 258 passing `multi-viewer-dynamic` worker tests + roost-smoke step13 + the cell render-stress above. A full claude-spawning multi-tab hammer on the live in-use machine was disproportionate for byte-identical code.
- Final sweep: `tsc` 4 baseline error-lines (0 new), `lint-roost` 0 violations, `bun test apps smoke` **643 pass / 0 fail**, over-cap (non-generated) = CellTerminal.tsx only.

---

## Finishing pass (post-review)

- **TS7 standardization.** Consolidated the dual toolchain onto **TypeScript 7** (`@typescript/native-preview` / `tsgo`): dropped classic `typescript@5.7` from root/web/worker, pointed all 5 app `typecheck` scripts + the CI gate at `tsgo`, added roost-cli's missing script, added root `@types/node@22` (dev-only, fixes the `node:net` typing so the base gate is 0 errors). Editor was already `useTsgo:true`; nothing imported the classic compiler API. CI typecheck gate now **passes** (was red on the 4 `net.Server` errors).
- **Modal dedup.** Extracted the shared docked-console shell → `apps/web/src/components/ConsoleModalShell.tsx`; `DeployConsoleModal`/`TransferConsoleModal` now pass their stream + header + sizes into it. Verbatim markup, behavior-preserving, tsgo-clean.
- **Unused-export down-scoping.** 49 knip-flagged exports made module-private (`export` removed, bodies kept — reversible, gate-protected). knip unused **exports 43→9, types 29→15**. Deliberately KEPT: db/schema.ts table types (14, schema surface), the `uiStore` context-menu cluster (7, active WIP), ed25519 `ParsedKey`/`parseSshEd25519Line`. Latent-orphan notes surfaced: `openDeployConsole` (nothing opens the deploy console) + `restartSync` (Reconnect handler uncalled) — down-scoped, flagged for owner.
- **ed25519 consolidation — KEPT (assessed, not deferred for effort).** The two `parseOpenSshEd25519` are algorithmically different (worker = self-described heuristic scan; coord = proper field-walking parser), so consolidation is a behavior change on the worker's boot-time key parse, and **`apps/worker/tests` has zero coverage of the parse/sign path** — an unverifiable change on the auth path (failure = fleet can't authenticate). Bounded DRY upside vs unbounded auth-outage downside. Safe path if ever wanted: add worker key-parse/sign test coverage FIRST, then consolidate into `@roost/shared/crypto` behind the full auth suite.
- Verified: 5-app `tsgo` 0 errors, base gate 0, `lint-roost` 0 violations, `bun test apps smoke` **643 pass / 0 fail**.
