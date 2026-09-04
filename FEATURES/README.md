<!-- AUDIENCE: claude -->
# Roost features — unified inventory

Canonical registry of the v0.5.0 release boundary and intentionally unavailable
surfaces. Named live source/tests are implementation authority; archive files
record lineage only.

## Status legend

- **SHIPPED:** included in the public self-hosted v0.5.0 release
- **QUALIFIED:** implementation passes its mandatory profile but is not
  publicly launched
- **BETA:** visible, intentionally limited surface with a documented
  alternative
- **PAUSED:** implementation work exists, but release qualification and
  publication are off
- **DEFERRED:** consciously postponed
- **DELETED:** superseded or based on a wrong premise

## v0.5.0 release boundary

| Status | Contract | Implemented truth | Authority |
|---|---|---|---|
| **SHIPPED** | Self-hosted host runtime | The coordinator/worker runtime is released and deployed on macOS arm64/x64 and Linux arm64/x64. Windows remains a browser client, not a released host. | `install-binary.sh`, `.github/workflows/release.yml` |
| **SHIPPED** | Automatic and direct HTTPS | No endpoint flags uses MagicDNS plus Tailscale Serve `:4102` → loopback `:4103`. All three direct flags make Bun terminate HTTPS on the selected port without calling Tailscale. Direct quickstart covers the coordinator, local worker, and browsers; current extra-worker enrollment remains Tailscale-gated. | `apps/roost-cli/src/quickstart-endpoint.ts`, `apps/roost-cli/src/add-machine.ts`, `join.sh` |
| **SHIPPED** | Durable session lifecycle | Worker-authored `opened`, `closed`, and `respawned` enter a bounded FULL-synchronous SQLite outbox, replay one at a time before snapshot/live, and leave only on the exact post-commit coordinator ACK. Coordinator event and projection commit atomically. | `apps/worker/src/transport/session-event-store.ts`, `apps/worker/src/transport/coord-link-unacked.ts`, `apps/coord/src/event-transaction.ts` |
| **SHIPPED** | Browser Sync recovery | Backfill uses a fixed event cutoff and bounded live tail; application ACK follows synchronous dispatch. Cold start/unprovable recovery uses guarded current-state hydration instead of pretending every state is recoverable from a browser cursor. | `apps/coord/src/connect/sync-feed.ts`, `apps/web/src/store/sync-flow.ts`, `apps/web/src/store/sync-bootstrap-hydration.ts` |
| **SHIPPED** | Cell terminal continuity | Worker `@wterm/core` owns terminal semantics; browsers receive generation-addressed full/delta cells. Visible panes escalate missed view proof or 20-second idle delivery through resync and a 10-second proof deadline to in-place Sync redial. Keeper adoption history is bounded to 1 MiB/channel. | `apps/web/src/store/terminal-stream-liveness.ts`, `apps/web/src/store/terminal-stream-view.ts`, `apps/worker/src/keeper/keeper-frame-handler.ts` |
| **QUALIFIED** | Managed dashboard isolation | Server-resolved membership scopes RPC, Sync, workers, and resources. Browser switches only after server confirmation, clears prior scoped state, and fences stale generations. | `apps/coord/src/connect/dashboard-authorization.ts`, `apps/web/src/store/dashboard-selection.ts`, `bun run test:managed` |
| **QUALIFIED** | Managed deployment | The Linux root-owned profile drives one exact-spec non-root coordinator container per operator-created account from an immutable digest, with distinct writable state/keys/credentials/route. Four E2E files contain five top-level cases. No production containers run; no managed image is published; the shared dashboard route is inactive; production email signup/Google auth are off. | `Dockerfile.coord`, `apps/roost-cli/src/saas/`, `apps/roost-cli/src/test.ts` |
| **BETA** | Global search | `/search` is informational and performs no global search. Use sidebar cwd/workspace filtering or per-terminal find over retained scrollback/live viewport. | `apps/web/src/components/MainPane.tsx`, `smoke/terminal/beta-surfaces.spec.ts` |
| **BETA** | Cross-worker transfer | The dialog performs no transfer RPC; retired public transfer endpoints return 404. Use terminal `rsync` or `scp`. | `apps/web/src/components/TransferDialog.tsx`, `smoke/terminal/beta-surfaces.spec.ts` |
| **PAUSED** | Windows coordinator/worker | v0.5.0 publishes and qualifies no Windows host assets or install/update path. | `.github/workflows/release.yml` |

## Historical implementation records — not current architecture

| Feature | Historical scope | Design | Record |
|---|---|---|---|
| **phase-24** | worker-outbound WebSocket seam | ../docs/archive/phase-24.md | Old hub superseded; raw protobuf WebSockets still carry long-lived browser Sync and worker links |
| **phase-25** | scrollback replay, worker restart, schema cleanup | ../docs/archive/phase-25.md | Legacy byte-replay lineage |
| **phase-26** | smoke backdoor and keeper pool | ../docs/archive/phase-26.md | Keeper pool survives; current Sync transport differs |
| **phase-ssb** | per-byte sequence splice | ../docs/archive/SEQNO-SPLICE.md | Sequence/ring remains in bounded keeper adoption; browsers receive cells |
| **phase-pb14/15/16** | parallel mount, priority, binary scrollback | — | Historical browser transport; current delivery is cell full/delta plus history pages |
| **phase-att1** | file attachment via path injection | ../docs/archive/phase-att1.md | `attachFile` unary RPC plus terminal drop/paste binding |
| **CONNECT-RPC** | unary RPC and protobuf conversion | ../docs/archive/CONNECT-RPC-MIGRATION.md | Connect owns unary RPC; raw protobuf WebSockets remain for long-lived streams |
| **T1.1** | drop H3 for `Bun.serve` native fetch | — | `cddc9c2f` |
| **T1.2** | typed SessionEvent and bus deltas | — | All 12 current SessionEvent kinds use `SessionEventProto`; coordinator JsonEvent emission is retired, while the legacy `FirehoseFrame.sessions` schema arm and coordinator/web receive compatibility remain |
| **T1.3** | OTEL tracing across coord, worker, web | — | W3C traceparent end to end |
| **T1.4** | reconnect backfill via event cursor | — | Current implementation adds fixed-cutoff recovery and guarded hydration |
| **T2.1** | multiplexed keeper foundation | — | Multiplexed pool is the only keeper mode |
| **T2.2** | in-band worker JWT rotation | — | Shipped |
| **T3.1** | multi-runtime coordinator factory and Node demo | — | Historical runtime seam |
| **T3.2** | headless coordinator E2E harness and bidi routing test | — | Shipped test infrastructure |

## Architecture decision archive

- `../docs/archive/SEQNO-SPLICE.md` — byte-sequence/ring design lineage
- `../docs/archive/PATHB-PARSER-CHOICE.md` — terminal-parser investigation
- `../docs/archive/PATHB-VT-PARSE.md` — server-side terminal-model notes

These documents are historical. Current authority is the worker-owned
`@wterm/core` model, generation-addressed cell delivery, and guarded history
paging described in the release table.

## Deleted plans

- `../docs/archive/MIGRATION-STATUS.md` — superseded by this inventory
- `att2-image-media-extensions.md` — never created; it assumed nonexistent
  wterm image APIs
- `ROADMAP.md` — never created; its transport dependency graph was wrong
- Neutralino desktop shell — superseded on released hosts by launchd and
  `systemd --user`; its dormant Windows branch is outside the v0.5.0 release.

---

**Last updated:** 2026-09-04

**Inventory authority:** this README for status; named live source and tests
for implementation.
