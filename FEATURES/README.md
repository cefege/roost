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
| **SHIPPED** | Coordinator HTTP/TLS front-door contract | The coordinator owns a single plaintext loopback listener (`ROOST_COORDINATOR_BIND=127.0.0.1:4103`, `ROOST_TRUST_PROXY=1`) and is told its public origin through `ROOST_WEB_PUBLIC_URL`. This one-front-door contract covers coordinator HTTP/TLS only: direct worker transport is loopback or coordinator-admitted authenticated WebRTC UDP, never a public worker HTTP/TLS listener. TLS, DNS, tunnels, and public reachability belong to the operator's front door; `roost quickstart --coordinator-url` is the only install shape, and worker enrollment resolves `ROOST_COORDINATOR_URL` → `ROOST_COORDINATOR_PUBLIC_URL` → `ROOST_WEB_PUBLIC_URL` or refuses. | `apps/roost-cli/src/quickstart-endpoint.ts`, `apps/roost-cli/src/add-machine.ts`, `apps/coord/src/bun-coordinator-listeners.ts`, `join.sh` |
| **SHIPPED** | Durable session lifecycle | Worker-authored `opened`, `closed`, and `respawned` enter a bounded FULL-synchronous SQLite outbox, replay one at a time before snapshot/live, and leave only on the exact post-commit coordinator ACK. Coordinator event and projection commit atomically. | `apps/worker/src/transport/session-event-store.ts`, `apps/worker/src/transport/coord-link-unacked.ts`, `apps/coord/src/event-transaction.ts` |
| **SHIPPED** | Browser Sync recovery | Backfill uses a fixed event cutoff and bounded live tail; application ACK follows synchronous dispatch. Sync remains terminal metadata/control, signaling, and fallback while an elected direct carrier transports terminal frames. Cold start/unprovable recovery uses guarded current-state hydration instead of pretending every state is recoverable from a browser cursor. | `apps/coord/src/connect/sync-feed.ts`, `apps/web/src/store/sync-flow.ts`, `apps/web/src/store/sync-bootstrap-hydration.ts` |
| **SHIPPED** | Automatic direct terminal transport | The browser starts Sync and elects per session: same-worker loopback, then a qualified coordinator-admitted authenticated WebRTC peer, then Sync fallback. Every visible selected pane names the baseline-qualified carrier as `Loopback`, `WebRTC`, or `Coordinator`; it shows `Waiting` while no current terminal screen proves a carrier. Peer configuration is strict and coordinator/worker scoped; it has no TURN service, automatic firewall change, or Tailscale management. A healthy established direct route may continue through coordinator outage until liveness or authorization ends; new negotiation, renewal, and fallback require the coordinator. | `apps/shared/src/config.ts`, `apps/shared/src/terminal-peer.ts`, `apps/worker/src/config.ts`, `apps/coord/src/connect/terminal-peer-negotiations.ts`, `apps/worker/src/terminal-peer-owner.ts`, `apps/web/src/ws/terminal-peer.ts`, `apps/web/src/store/terminal-stream-transport.ts`, `apps/web/src/store/local-transport-indicator.ts`, `apps/web/src/components/TerminalTransportIndicator.tsx` |
| **SHIPPED** | Cell terminal continuity | Worker `@wterm/core` owns terminal semantics; browsers receive generation-addressed full/delta cells. A direct route commits only after its validated full baseline; direct loss repairs through a fresh Sync baseline when the coordinator is reachable. Visible panes escalate missed view proof or 20-second idle delivery through resync and a 10-second proof deadline. Keeper adoption history is bounded to 1 MiB/channel. | `apps/web/src/store/terminal-stream-liveness.ts`, `apps/web/src/store/terminal-stream-promotion.ts`, `apps/web/src/store/terminal-stream-view.ts`, `apps/worker/src/keeper/keeper-frame-handler.ts` |
| **SHIPPED** | Single-install tenancy invariant | Coordinator boot creates and validates the single local account/organization/dashboard before any listener opens, and refuses to start otherwise. Authorization is per credential: an authenticated browser device reaches the whole install, a worker JWT reaches only its own resources, and revocation closes every socket for that fingerprint. | `apps/coord/src/self-hosted-tenant.ts`, `apps/coord/src/connect/auth-principal.ts`, `apps/web/src/store/auth-boundary.ts` |
| **QUALIFIED** | Global search | `/search` combines the shared metadata projection with coordinator-authorized literal search across up to 32 open sessions' retained terminal rows. Results page through opaque cursors, preserve typed per-session partials, and rerun pane-local find against the current grid epoch before reveal; attention scope remains metadata/status-only. | `apps/coord/src/connect/handlers-sessions-global-search.ts`, `apps/worker/src/terminal-search-batch.ts`, `apps/web/src/lib/globalContentSearchController.ts`, `smoke/terminal/global-search.spec.ts` |
| **SHIPPED** | Browser-local pane layouts | Per-tab state reports and exact-target `UiApplyLayout` use one resource-bounded typed `LayoutDocumentV1` parser with deterministic preorder leaf/slot keys; runtime IDs and unknown protobuf fields never cross the boundary. The RPC revalidates the browser's URL-active folder and live sessions, rejects client-only optimistic membership, mints runtime IDs afresh, and commits once; remote apply attempts focused-selection navigation before `applied`, which proves the commit rather than navigation completion. The CLI resolves one reported fingerprint/tab tuple. Sync rejects tab IDs over 256 UTF-8 bytes, and the RPC accepts a frame only for the current tab, socket, and nonempty correlation. | `apps/shared/src/layout-document.ts`, `apps/web/src/store/paneLayoutDocument.ts`, `apps/web/src/lib/uiStateReport.ts`, `apps/web/src/lib/uiLayoutApplyCore.ts` |
| **SHIPPED** | Volatile agent status, waits, and fenced prompts | Device-authorized Get/List/Wait RPCs and `roost api agent-status` / `agents` / `agent-wait` expose current worker-observed state and exact-occupant waits. `SessionsPrompt` / `roost api agent-prompt` may send one ≤16 KiB text input to the same ordinary shell PTY only while exact integration epoch, occupant, revision, live process proof, and `idle|working` state still match; raw `SessionsInput` is unchanged. Input and optional wait outcomes stay separate, ambiguous writes are never retried, and prompt text and status messages are never logged, audited, or stored. Roost owns no agent process, conversation, transcript, tool call, or approval model. | `apps/shared/src/terminal-input.ts`, `apps/coord/src/connect/agent-prompt-control.ts`, `apps/worker/src/agent-prompt-control.ts`, `apps/roost-cli/src/api-agent-prompt.ts`, `smoke/terminal/agent-prompt.spec.ts` |
| **SHIPPED** | Private OMP conversation references | The official OMP integration reports `session_file` (preferred) or `session_id` through a separately acknowledged, capability- and PID-ancestry-attested local method. Set/replace/clear enter the durable worker SessionEvent outbox; the coordinator folds worker `client_seq` into private recovery metadata that snapshots cannot erase and only the owning worker can read. Raw opaque values never enter public Session, browser Sync/live/backfill/cutoffs, CLI session output, search, logs, or audit. | `apps/shared/src/agent-conversation-reference.ts`, `apps/worker/src/agent-status/report-server.ts`, `apps/coord/src/agent-conversation-recovery.ts` |
| **PAUSED** | Automatic OMP conversation restoration | The implementation is present behind strict worker-local `ROOST_AGENT_CONVERSATION_RESTORE=0|1`. It defaults to disabled everywhere; explicit `1` is POSIX-only and Windows rejects it. After failed keeper adoption and durable ordinary-shell respawn, the worker types one fixed `omp --resume=<reference>` command as a single acknowledged CR-terminated batch, with the opaque value canonically quoted into exactly one argv element; duplicate references are skipped, and accepted, rejected, and ambiguous outcomes never retry and retain the reference. Default-on remains blocked until actual official-OMP POSIX real-stack qualification is completed. | `apps/worker/src/agent-conversation-restore.ts`, `ARCHITECTURE.md` |
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

**Last updated:** 2026-09-20

**Inventory authority:** this README for status; named live source and tests
for implementation.
