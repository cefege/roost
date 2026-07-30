# Roost — Triple Audit Ledger (bloat + correctness/security/perf + load-under-traffic)

- **Date:** 2026-07-23
- **Codebase SHA:** `65686dc5`
- **Scope:** whole Roost codebase (`apps/{coord,web,worker,shared,roost-cli}` + `smoke/`) through three lenses: (1) bloat/over-engineering, (2) correctness/security/perf bugs, (3) load-under-traffic hot paths.
- **Mode:** READ-ONLY. Report-only — no fixes applied, no working-tree changes.
- **Delta basis:** measured against the prior ledger at [`docs/archive/AUDIT.md`](archive/AUDIT.md). Net-new findings since that pass are tagged `[NEW]`; re-verifications of its BACKLOG items are tagged `[BACKLOG-REVERIFY]`. Nothing identical-and-resolved is relisted as new.
- **Method:** 9 read-only `scout` agents (disjoint file sets) + integrator verification. Every finding carries a `file:line` anchor read this session (by a scout and/or the integrator). Integrator personally re-verified the headline P1, both P2 races, the SPA store-mutation cluster, and the two knip false-positives.
- **Excluded:** `apps/shared/src/gen/**` (protobuf), `mockups/**`, `docs/archive/**`, `.claude/**`, `scripts/**` (generated / frozen / tooling — per `knip.json` ignore).
- **Scale assumption:** single-user fleet (README: "single-user today"). Load findings cited in (sessions × browsers × hours), not multi-tenant SaaS scale.

> **Historical snapshot:** this ledger records the 2026-07-23 codebase. The
> later terminal-only cutover retired every structured agent path, including
> detection/status projection and agent transcript UI. References explicitly
> marked retired below are evidence from that audit, not current architecture.

---

## Headline

**No P0. One P1 (compound, load-amplified): a single slow browser can stall session-event writes fleet-wide.** Otherwise the codebase is mature: the recurring-failure catalog (`CLAUDE.md` L11) is well-respected, all prior BACKLOG re-verifications either resolved or improved, and memory is bounded for week-long uptime. The remaining debt is (a) the P1 fan-out/transaction chain, (b) unbounded on-disk growth (`events`, `audit_log`, `push_subscriptions`), and (c) `CellTerminal.tsx` regressing further over the file cap.

---

## Lens 1 — Bloat / over-engineering

Tag set: `delete` / `stdlib` / `native` / `yagni` / `shrink`. Format: `tag  what-to-cut. replacement. [path:line]`.

1. `shrink`  **Duplicate `*Deps` interfaces.** `CoordDeps` and `ConnectDeps` are byte-identical (`{ db, coordKey, cfg, jwtCache }`); `createCoord(deps)` passes `deps` straight into `buildConnectRouter(deps)`. Import one type from the other. `[NEW]` `apps/coord/src/coord-factory.ts:32` ↔ `apps/coord/src/connect/router.ts:35` (~6 lines)
2. `shrink`  **`relativeTime` duplicated.** Identical implementation in two settings panes. Extract to `apps/web/src/lib/format.ts` (already the `formatBytes` home). `[NEW]` `apps/web/src/components/Settings/WebhooksPane.tsx:19` ↔ `apps/web/src/components/PermissionRuleEditor.tsx:200` (2 sites, ~10 lines)
3. `shrink`  **`@types/qrcode` misplaced.** Listed under `dependencies`; it's a build-time type package → belongs in `devDependencies`. `[NEW]` `apps/web/package.json`
4. `yagni`   **`DesignGallery.tsx` dev showcase.** 323-LOC lazy-loaded `/design` page is the sole consumer of the `Surface`/`Sheet`/`SectionTitle` md primitives. If `/design` is developer-only, co-locate those primitives instead of keeping them in the shared barrel. `[NEW]` `apps/web/src/components/DesignGallery.tsx`
5. `delete`  **Unused export `PredictMode`.** knip-flagged, no external consumer. Down-scope (drop `export`) or delete. `[NEW]` `apps/web/src/lib/predictPref.ts:31`
6. `delete`  **Unused export `supportsViewTransitions`.** knip-flagged, no consumer. `[NEW]` `apps/web/src/lib/viewTransition.ts:17`
7. `shrink`  **`CellTerminal.tsx` regressed over the file cap.** Grew **873 → 1389 LOC** (+59%) since the prior audit; still the only over-cap hand-written file (`src/gen/**` is cap-exempt). The prior audit BACKLOG'd it as unsplittable (closures over shared mutable Solid state); the growth makes re-assessment more urgent, not less. `[BACKLOG-REVERIFY, worsened]` `apps/web/src/components/CellTerminal.tsx`
8. `delete` *(deferred)*  **`uiStore` context-menu cluster (6 exports).** `openContextMenu`, `dismissContextMenu`, `startRename`, `stopRename`, `toggleSidebar`, `openNotificationBell` — knip-flagged. Prior audit KEPT as "active WIP"; still 6 (was 7). Confirm WIP still active or down-scope. `[BACKLOG-REVERIFY]` `apps/web/src/store/uiStore.ts:79-87`
9. `shrink`  **`solid-js` unlisted in `tsconfig.base.json`.** Cosmetic knip hint; prior audit kept as false-positive (root tsconfig references it for `jsxImportSource`). `[BACKLOG-REVERIFY, false-positive]` `tsconfig.base.json`
10. *(debt marker)*  **`TODO R4.3: Global search`** still unimplemented (was `MainPane.tsx:145`, now `:166`). `[BACKLOG-REVERIFY]` `apps/web/src/components/MainPane.tsx:166`

**knip false-positives confirmed (do NOT delete):**
- `apps/web/public/sw-push.js` — runtime-registered service worker: `push-client.ts:13,42` (`navigator.serviceWorker.register("/sw-push.js")`). knip is blind to URL-registered SWs.
- `@resvg/resvg-js` (root devDep) — imported by `scripts/gen-icons.ts:6`; knip ignores `scripts/**`.
- ed25519 `ParsedKey`/`parseSshEd25519Line` — auth surface, kept (prior audit §6).
- `apps/coord/src/db/schema.ts` table types (14) — Drizzle schema typing surface (prior audit §1).

**`net:` rollup — ~20–30 lines removable (Deps dedup + `relativeTime` + `qrcode` move), 2 export down-scopes. Zero real dependencies removable (both knip deps are false-positives). The substantive Lens-1 signal is `CellTerminal.tsx`'s +516-line regression against the 400-line cap.**

---

## Lens 2 — Correctness / security / perf

Severity: **P0** data-loss/auth-bypass/security-hole/grid-corruption · **P1** bug under common conditions/crash/leak · **P2** edge-condition/moderate-scale perf · **P3** smell/extreme-scale perf. `sec:` prefix = security. Format: `severity  file:line  what  why-it-bites  suggested-direction`.

### P1

1. **P1  slow-browser → fleet-wide session-event write stall (compound).** `apps/coord/src/event-log.ts:305` publishes `sessionBus.publish(event)` **inside** the Kysely transaction (also L231 for snapshot). `apps/coord/src/buses.ts` `BoundedBus.publish` iterates subscribers **synchronously** (for-of, no backpressure, no bounded queue). `apps/coord/src/connect/sync-ws-handler.ts` does `ws.send(toBinary(...))` and **ignores Bun's boolean return** (no backpressure). Chain: one slow browser (phone on a flaky tailnet link) → its `ws.send` blocks → synchronous `publish` blocks → `appendEvent`'s txn stays open → SQLite write lock held → **every** `appendEvent` across the fleet queues behind it → all session events stall. WAL mode (`db/connection.ts:18`, `busy_timeout=5000`) does not help: only one writer. `[NEW] (also lens-3)` Suggested direction: move `bus.publish` **outside** the txn (publish after commit; stamp `_event_id` before), and make `BoundedBus.publish` non-blocking with a per-subscriber bounded queue + drop-on-overflow + diag signal.

### P2

2. **P2  signal-killed PTY leaves a zombie session.** `apps/worker/src/session-emit.ts:280-285` — `onExit` early-returns on `exitCode === null` (Bun signals a signal-death as null exit code) without calling `closedByKeeper`, so the FSM never transitions to closed and the session row persists as "live". The `diag("session.exit_null")` at L283 shows awareness but there is no auto-recovery. Note: the *other* `onExit(null)` path — `keeper-pool-lifecycle.ts` calling `onExit(null)` for all channels when the keeper socket dies — is **intentional** (sessions survive for re-adoption on reconnect); only single-child signal-death (external `kill -9`, OOM, or a force-✕ routed via signal) zombies. `[NEW]` Suggested direction: on `exitCode === null` with the keeper still alive, treat as a closed event (or reap via a stuck-in-non-terminal-state watchdog). `[unverified: whether the force-✕ button routes through signal-death — if so, elevate to P1]`
3. **P2  `push_subscriptions` silent-unsubscribe leak + per-dispatch crypto tax.** `apps/coord/src/push-sender.ts` prunes subscriptions only on HTTP 404/410; a browser that unsubscribes silently (uninstalls, disables notifications, clears storage) leaves a row forever. `apps/coord/src/push-dispatch.ts` then `selectAll()` on every push dispatch, paying RFC-8291 encryption cost per leaked row. `[NEW] (also lens-3, sec-hygiene)` Suggested direction: periodic reaper (e.g. mark subscriptions stale after N failed sends or a TTL probe).
4. **P2  `events` table append-only, no retention/compaction.** No `DELETE FROM events`, no `VACUUM`, no TTL trigger in `migrations/0001_init.sql`. Growth = events/sec × hours × days; SQLite file grows monotonically. `[NEW] (also lens-3)` `apps/coord/src/event-log.ts` + `apps/coord/migrations/0001_init.sql` Suggested direction: periodic archival/compaction or a rolling TTL on old events the SPA no longer backfills (see backfill cap, finding 9).
5. **P2  `audit_log` table append-only, no retention.** Same class as `events`; written per-RPC in the auth interceptor. `[NEW] (also lens-3, sec)` `apps/coord/src/connect/auth-interceptor.ts` Suggested direction: TTL or cap; audit logs are regulatory but a single-user fleet doesn't need unbounded online retention.
6. **P2  web per-session maps never reaped.** `apps/web/src/lib/diag.ts:22-35` `_sessionTrace` and `apps/web/src/ws/input-channel.ts:46,70` `_lastSendTs`/`dropTotals` grow one entry per session ever seen, for the tab lifetime. `apps/web/src/lib/leakWatch.ts` is observation-only and misses exactly these (plus `_oscBuffer`, `_bytesHandlers`). `[NEW]` Suggested direction: a single session-close reaper hook (mirror `projector.ts:64-72`'s closed-event cleanup) for all per-session maps; extend `leakWatch` to track them.
7. **P2  backfill cap=1000 silent gap, no client error.** `apps/coord/src/event-log.ts:76-90` `getEventsSince(..., limit=1000)`: a browser reconnecting after >1000 missed events gets only the newest 1000 with no gap signal to the SPA beyond a diag counter. The live stream then resumes from the newest id, so the gap is permanent and silent. `[NEW]` `apps/coord/src/connect/handlers-streaming.ts` Suggested direction: emit an explicit "gap" firehose frame when `sinceId` is older than the oldest retained row.
8. **P2  `sessionsAssignWorkspace` split-transaction consistency window.** `apps/coord/src/connect/handlers-sessions.ts:311-351` writes the `workspace_assigned` event via `appendEvent` (its own txn) and the `workspace_sessions` junction in a **separate** txn (L330). Between the two commits, `sessions.workspace_id` (column) and the junction transiently disagree. It self-heals (the `sessions-set` publish at L349 re-reads the post-commit junction), so impact is a brief UI flicker / double-count, not corruption. `[NEW]` Suggested direction: fold the junction write into `appendEvent`'s transaction, or document the column/junction dual-write invariant.
9. **P2  `CoordLink` unacked-overflow silent eviction.** `apps/worker/src/transport/CoordLink.ts` — the pending/unacked queue (cap ~8192) silently evicts the oldest event on overflow during a sustained disconnect. For `closed` events this is data loss (a closed session may never get its tombstone if the worker was disconnected long enough). D-4b at-least-once + snapshot reconcile cover most cases, but the eviction is unconditional. `[NEW, static]` Suggested direction: never evict terminal events (`closed`), or force a snapshot on reconnect-after-overflow.
10. **P2  worker-keeper binary-mode `pgrep` mismatch + keeper listen-error no-exit.** `apps/worker/src/boot-keeper.ts` pgrep pattern matches only the source-mode keeper argv; the binary-mode `roost keeper <sock>` argv is never matched → a stale keeper from a prior build isn't detected. Separately, `apps/worker/src/keeper/multiplexed-main.ts:104` `server.on('error')` logs but does not exit → a listen failure (e.g. socket-in-use) leaves a zombie keeper process. `[NEW]` Suggested direction: match the binary argv; exit the keeper on listen `error`.
11. **P2  `attachment-reaper` manifest-orphaned dirs.** `apps/worker/src/attachment-reaper.ts:68,84` skips `MANIFEST_NAME` in its sweep but the empty-dir check then never matches a dir containing only a manifest → session dirs leak. `[NEW]` Suggested direction: reap dirs whose only remaining file is the manifest.
12. **sec: P2  CSP allows `unsafe-inline` in `script-src`.** `apps/coord/src/middleware/security.ts` `CSP_BASE` — `unsafe-inline` weakens XSS defense. Solid's runtime is DOM-templated (no `eval`), so a nonce/hash-based CSP is feasible. `[NEW]` (mitigated if `relaxedCsp` is off by default — `config.ts` default is `false`).

### P3

13. **sec: P3  CORS wildcard default.** `apps/coord/src/middleware/security.ts` — `corsAllowedOrigins` defaults to `[]` (wildcard). Low real risk on a single-user tailnet; tighten if multi-user is ever enabled. `[NEW]`
14. **sec: P3  trace-id taken from untrusted header, logged raw.** `apps/coord/src/connect/auth-interceptor.ts` reads `x-roost-trace-id` and writes it to `audit_log`/logs verbatim → log-injection / log-spoofing vector. `[NEW]` Suggested direction: sanitize (length + charset) before storing.
15. **sec: P3  `migrate.ts` string-interpolated migration-name INSERT.** `apps/coord/src/db/migrate.ts:49` interpolates the migration filename into an INSERT; SQL-injection-via-filename is structurally possible. Low severity (migrations are local files, never attacker-controlled) but parameterize for hygiene. `[NEW]`
16. **sec: P3  push subscription key-validation coverage gap.** `apps/coord/src/push-sender.ts` delegates RFC-8291 to `web-push`; confirm p256dh/auth key lengths are validated before the (costly) encrypt attempt. `[NEW, partial — encryption itself is the library's contract]`
17. **P3  `sync-bootstrap` plain-merge vs `reconcile` inconsistency.** `apps/web/src/store/sync-bootstrap.ts:284,300,318,332,343` use `setRootStore("k", rec)` (Solid nested **merge**), whereas the `workers` bootstrap (L153/L263) uses per-key `reconcile(w)`. The merge form is safe today only because the `synced` guard makes bootstrap run once on an empty store. If bootstrap ever re-runs (e.g. a future re-hydrate), server-deleted rows would **not** be pruned from the SPA. `[NEW]` (This was initially reported as a Solid `setStore` no-op P1 — **rejected on verification**: `setRootStore` is raw `setState` (`root.ts:91`); the L11 `feedback_solid_setstore_record_replace` no-op applies only to the 2-arg function-updater form `setStore(recordKey, fn→newRecord)`, which does **not** appear in this codebase. The mutations module even cites the L11 rule in comments. Codebase is clean of the anti-pattern.)
18. **P3  stale `BUG` comment.** `apps/web/src/lib/keyboardShortcuts.ts:28-46` documents a bug that is already fixed; misleading for future readers. Delete the comment. `[BACKLOG-REVERIFY, resolved-as-stale]`
19. **P3  `_rebuildWtermCore` narrow race.** `apps/worker/src/session-viewport.ts` — between `fresh.writeRaw(scrollback)` and the `wtermCore` swap, a concurrent byte handler could write to the old core. Window is tiny (single sync block); flagged for completeness. `[NEW, static]`
20. **P3  per-chunk O(ring) allocation in scrollback append.** `apps/worker/src/session-scrollback.ts` allocates/copies the full `SCROLLBACK_CAP_BYTES` (1 MB) ring on every PTY output chunk append. Steady GC pressure on high-output sessions. `[NEW]`
21. **P3  `_parseOsc7` writes `sessions.cwd` from the byte dispatch.** `apps/web/src/store/sync-dispatch.ts:97` — a store write from the byte layer bypasses the projector (which delegates to shared `foldEvent`). Correct Solid form (per-key/per-field), not a mutation violation, but an architectural aside: OSC7-detected cwd is a separate path from event-sourced cwd. `[NEW]`

### Verified safe (resolved verify-items — NOT findings)

- **`sinceEventId` replay ordering is correct.** `handlers-streaming.ts` subscribes to the bus **before** issuing the backfill SELECT; `yieldedSessionIds` dedup prevents double-delivery. `[BACKLOG-REVERIFY, confirmed-safe]`
- **All mutation handlers publish to their bus after writes.** tasks/workspaces/webhooks/permissions/mcp/pair/workers all `publish*State(row)` post-UPDATE — the `feedback_task_state_delta_only_created` L11 leak is fixed everywhere. `[BACKLOG-REVERIFY, confirmed-safe]`
- **`tasksNextPending` claim is atomic.** `handlers-tasks.ts:74-79` is a single `UPDATE...WHERE id=(subquery)`; SQLite is single-writer with statement-level atomicity, so the subquery sees the prior claim and two workers pick distinct rows. *(Initially reported as a P2 race — rejected on verification.)*
- **`onExit`/keeper-outlives-worker, FSM terminal-state + invalid-transition rejection, protocol-v2 codec safety, snapshot ordering, ed25519 `parseOpenSshEd2551919` (both impls, no parse bug), worker-ws `fromBinary` try/catch + buffer copy, loopback NOT spoofable (TCP peer, header stripped-then-reset), DOM render well-bounded, all volatile coord maps bounded, memory overall bounded for week+ uptime, projector delegates to shared `foldEvent`, components never mutate store directly.** All `[BACKLOG-REVERIFY, confirmed-safe]`.

---

## Lens 3 — Load-under-traffic (static)

Static analysis of hot paths for contention, unbounded growth, per-event allocation, backpressure. Format: `hot-path  file:line  bottleneck  scale-at-which-it-bites  suggested-direction`. Empirical load not measured (out of scope); plausible-but-unmeasured findings marked `[static, unmeasured]`.

1. **[P1] slow-browser → fleet-wide write stall.** *(see Lens-2 finding 1 — the headline load finding)*. `event-log.ts:305` × `buses.ts` × `sync-ws-handler.ts`. Bites at **N ≥ 1 slow browser**; worst case = total write starvation while the browser's TCP window is full.
2. **RETIRED — detect-sweep CPU.** The audited structured-status detector (`apps/worker/src/detect/`) was removed by the terminal-only cutover; its former scaling finding is historical and no longer applies.
3. **events/audit_log on-disk growth.** *(see Lens-2 findings 4–5)*. Bites in **days–weeks** of uptime; SQLite file size + backfill cost grow monotonically.
4. **copy-per-viewer proto encode + no `permessage-deflate`.** `apps/coord/src/connect/handlers-streaming.ts` cell fan-out calls `toBinary(FirehoseFrameSchema, f)` **per Sync subscriber** per cell frame (no shared encode); `apps/coord/src/main.ts` `Bun.serve` has no `permessage-deflate` → cell grids ship uncompressed. Bites at **N ≥ 3–5 browsers** on a constrained tailnet uplink. Direction: encode once, broadcast bytes; enable permessage-deflate (note: `connect-node` zlib segfaults under Bun — L11 — but the raw worker/browser WS paths are Bun-native and deflate-safe).
5. **`BoundedBus.publish` O(capacity) `Array.shift`.** `apps/coord/src/buses.ts` — write-only ring uses `Array.shift` (O(n)) on overflow; caps are 512 (`globalBytesBus`) / 64 (`globalCellBus`). Per-publish O(cap) at high frame rates. Bites at **hundreds of frames/sec**. Direction: circular-buffer index instead of shift.
6. **backfill replay blocks the live stream.** `handlers-streaming.ts` + `event-log.ts:76-90` — a reconnect after a long gap replays up to 1000 events with `JSON.parse` per row, with no progress signal, before the live stream flows. Bites on **reconnect after >1000 missed events** (a long sleep / network gap). Direction: interleave backfill and live, or cap + explicit-gap signal (finding 7).
7. **`globalBytesBus` ring pins `Uint8Array` refs.** `apps/coord/src/buses.ts` — drop-oldest cap 512 pins up to ~512 × chunk-size of raw bytes (worst case single-digit MB). Bounded, but the bytes bus is the highest-chunk-rate. Direction: lower cap or weak-ref if memory-tight. `[static, unmeasured]`
8. **SHA-256 per chunk on both input paths.** `apps/web/src/ws/input-channel.ts` (per-batch `crypto.subtle.digest`) and `apps/worker/src/main.ts` `_workerSha8` (per-chunk `createHash`). Diagnostic fingerprints cost a hash per keystroke batch and per PtyOut chunk. Bites at **high typing rate × many sessions**. Direction: sample or drop if the diag signal is rarely consumed.
9. **scrollback full-replay on deliberate resize.** `apps/worker/src/session-viewport.ts:296` (`ponytail:`) replays the ≤1 MB raw ring per session on resize (OPT2 server-side-grid model — correct for fidelity). Bites as **O(sessions)** on a bulk resize (e.g. window-manager cascade). Bounded per-session; direction is the documented Path B if it regresses.

**Memory verdict:** RSS is bounded for week-long uptime — all in-process structures are capped (scrollback 1 MB/session + 1 MB/channel keeper ring; BoundedBus drop-oldest rings; volatile coord maps pruned on `closed`/TTL; viewer-tracker 10 s TTL; deploy-jobs 5 min GC; web IndexedDB single-row). **The only unbounded growth is on disk** (`events`, `audit_log`, `push_subscriptions`).

---

## Delta table — NEW vs BACKLOG-REVERIFY

| # | Finding | Tag | Lens |
|---|---|---|---|
| L2-1 | slow-browser → fleet-wide write stall (compound) | `[NEW]` | 2+3 |
| L2-2 | signal-killed PTY zombie session | `[NEW]` | 2 |
| L2-3 | `push_subscriptions` silent leak + crypto tax | `[NEW]` | 2+3 |
| L2-4/5 | `events` / `audit_log` append-only, no retention | `[NEW]` | 2+3 |
| L2-6 | web per-session maps never reaped; leakWatch gap | `[NEW]` | 2 |
| L2-7 | backfill cap=1000 silent gap | `[NEW]` | 2 |
| L2-8 | `sessionsAssignWorkspace` split-txn window | `[NEW]` | 2 |
| L2-9 | `CoordLink` unacked-overflow silent eviction | `[NEW]` | 2 |
| L2-10 | binary-mode pgrep mismatch + keeper listen-error no-exit | `[NEW]` | 2 |
| L2-11 | attachment-reaper manifest-dir leak | `[NEW]` | 2 |
| L2-12 | CSP `unsafe-inline` | `[NEW]` | 2 sec |
| L2-13–16 | CORS wildcard / trace-id log-injection / migrate.ts interpolation / push key-validation | `[NEW]` | 2 sec |
| L2-17 | sync-bootstrap plain-merge vs reconcile | `[NEW]` | 2 |
| L2-18 | stale BUG comment | `[BACKLOG-REVERIFY → resolved-stale]` | 2 |
| L2-19–21 | rebuildWtermCore race / scrollback per-chunk O(ring) / OSC7 cwd path | `[NEW]` | 2 |
| L1-7 | `CellTerminal.tsx` 873→1389 LOC | `[BACKLOG-REVERIFY, worsened]` | 1 |
| L1-8 | uiStore cluster (6 exports) | `[BACKLOG-REVERIFY]` | 1 |
| L1-1–6,9–10 | Deps dup / relativeTime / qrcode / DesignGallery / 2 exports / solid-js tsconfig / Global-search TODO | `[NEW]` / `[BACKLOG-REVERIFY]` | 1 |
| — | `openDeployConsole` + `restartSync` latent orphans | `[BACKLOG-REVERIFY → RESOLVED]` | — |
| — | `@wterm/core` in web deps | `[BACKLOG-REVERIFY → RESOLVED]` | — |
| — | modal dedup (`ConsoleModalShell`) | `[BACKLOG-REVERIFY → holds]` | — |
| — | 49 prior down-scoped exports | `[BACKLOG-REVERIFY → improved: knip 7+1 now]` | — |

---

## Coverage & confidence

**Read this session (anchors verified):** `event-log.ts` (full `appendEvent`, integrator-read), `handlers-tasks.ts:71-101`, `handlers-sessions.ts:311-385`, `session-emit.ts:280-285`, `sync-bootstrap.ts:262-360`, `sync-handlers.ts:49-133`, `store/{root,mutations,projector}.ts` (grep), `coord-factory.ts`/`router.ts` Deps, `push-client.ts` (SW registration), `scripts/gen-icons.ts` (resvg), plus all 9 scout file-survey reads.

**knip corroboration (`bun run knip`, this session):** unused exports **7** (down from prior 9), unused types **1** (down from 15), unused files **1** (`sw-push.js` — false positive, runtime SW), unused devDeps **1** (`@resvg/resvg-js` — false positive, `scripts/**`). Continuous improvement since the prior audit; no regression. The two knip "unused" items are both confirmed false-positives (documented above).

**Spot-check (≥5 random findings resolved to real lines, integrator-verified):** `event-log.ts:305` (publish-in-txn), `session-emit.ts:280` (onExit null), `handlers-tasks.ts:74` (atomic claim), `sync-bootstrap.ts:284` (merge form), `handlers-sessions.ts:330` (junction txn), `buses.ts` publish loop, `push-client.ts:42` (SW register). All grounded.

**Excluded:** `apps/shared/src/gen/**` (protobuf), `mockups/**`, `docs/archive/**`, `.claude/**`, `scripts/**` — generated / frozen / tooling.

**Not measured (out of scope per plan):** empirical load — no live coord+worker+browser run under load. All Lens-3 findings are static; the P1 chain is grounded in code but its real-world trigger frequency (how often a browser's `ws.send` actually blocks long enough to stall the txn) is `[static, unmeasured]`. Per the plan's contingency, if this P1 is contested before any fix, run `roost-render-stress` or a SQLite write-loop micro-bench to confirm.

**Rejected findings (verified false-positive, kept for traceability):**
- `sync-bootstrap.ts:284` / `sync-handlers.ts:70,89` as Solid `setStore` Record-no-op (L11) — wrong call shape; codebase is clean of the anti-pattern.
- `handlers-tasks.ts:74` `tasksNextPending` race — SQLite single-writer makes the atomic `UPDATE...subquery` safe.
- knip `sw-push.js` / `@resvg/resvg-js` unused — both have runtime/script consumers knip can't see.

**Confidence:** high on Lens 1 and the verified-safe items; high on the P1 chain (three independent reads agree); medium on Lens-2 edge-condition bugs (2C/2D) that depend on trigger-frequency not measured here.
