# @roost/shared — the wire source of truth

Every shape that crosses a process boundary is defined here once: Zod schemas
for in-app validation, protobuf for the bytes, and adapters between them. Coord,
worker, web, and the CLI all import from this package; nothing here imports back.

Path references are relative to `apps/shared/` unless they start at the repo root (`apps/…`, `scripts/…`, `smoke/…`, `docs/…`).

## Import rule: subpath-only

**`import { X } from "@roost/shared"` does not resolve.** The package barrel
module and the `"."` entry in `package.json#exports` are both deleted. Import the
subpath that owns the symbol.

The removed barrel exposed only a subset of modules with no stated rule, so a
bare import silently resolved for some symbols and failed for others. One
import style is now correct instead of two.

`@roost/shared` is still the valid **package** name, so
`bun run --filter='@roost/shared' proto:gen` remains correct. It is only the `"."`
*export* that is gone.

| Subpath | Supplies |
| --- | --- |
| `@roost/shared/wire` | Zod schemas + `z.brand()` identity types + `foldEvent`/`foldAll` |
| `@roost/shared/wire/event-proto` | `eventToProto` / `protoToEvent` |
| `@roost/shared/wire/coord-worker` | worker↔coordinator WebSocket schemas, auth subprotocol, PTY directions |
| `@roost/shared/wire/session-proto` | Session ↔ proto |
| `@roost/shared/wire/row-proto` | scrollback row ↔ proto |
| `@roost/shared/wire/sync-ws` | Sync WebSocket path, auth subprotocol, negotiation query constants |
| `@roost/shared/wire/headers` | shared `x-roost-*` header names and listener-trust sentinel values |
| `@roost/shared/terminal-search` | bounded paging limits, Unicode code-point utilities, stop reasons, worker-result validation |
| `@roost/shared/terminal-input` | terminal newline/paste encoding plus guarded-prompt byte and wait bounds |
| `@roost/shared/layout-document` | portable v1 pane-tree types, one resource-bounded strict parser, and inclusive split-ratio bounds |
| `@roost/shared/layout-document-proto` | preflighted, validated `LayoutDocumentV1` ↔ protobuf recursion adapter |
| `@roost/shared/agent-conversation-reference` | bounded private OMP reference + sequence-aware recovery fold |
| `@roost/shared/agent-conversation-reference-proto` | strict reference/recovery metadata ↔ protobuf adapters |
| `@roost/shared/ui-state` | allocation-free UTF-8 measurement plus UI report text, cardinality, and identity-rate limits |
| `@roost/shared/cell` | cell-grid model, emitter, delta apply, bounded snapshot chunking/assembly (R11) |
| `@roost/shared/cell/cell-proto` | cell frame ↔ proto |
| `@roost/shared/proto/*` | every generated `_pb.ts` (`…/proto/coordinator_pb`) |
| `@roost/shared/config` | `CoordConfig` + `loadCoordConfig(env)` — **coord only** |
| `@roost/shared/tenant-route` | lowercase 64-hex tenant route-key validation |
| `@roost/shared/paths` | per-platform data/log/service dirs + service labels |
| `@roost/shared/shell-quote` | canonical POSIX single-quote encoding |
| `@roost/shared/platform` | `SupportedHostPlatform`, `supportedHostPlatform()`, `assertNeverPlatform` |
| `@roost/shared/native-path` | lexical worker-path normalization (browser-safe) |
| `@roost/shared/tailnet` | tailscale binary candidates + MagicDNS name resolution |
| `@roost/shared/fingerprint` | `fingerprintOf` — the one pubkey fingerprint |
| `@roost/shared/durability` | `durableWriteFile` atomic write + private DACL |
| `@roost/shared/email-client` | provider-neutral Resend client + classified outcomes |
| `@roost/shared/email-payload` | AES-256-GCM persisted email-outbox payload boundary |
| `@roost/shared/retry` | capped exponential-backoff delay + jitter policy |
| `@roost/shared/jwt-base` | Node-side JWT base64url codec; never browser-imported |
| `@roost/shared/native-credentials` | account-email normalization + native-password policy |
| `@roost/shared/local-endpoint` | UDS / named-pipe prep, securing, capability tokens |
| `@roost/shared/service-health` | local health server + prober (re-exports its protocol schemas) |
| `@roost/shared/build-identity` | compiled-binary version + git sha |
| `@roost/shared/machine-join-command` | the enrollment command `roost add-machine` prints |
| `@roost/shared/windows-helper` | typed wrappers on `roost-win-helper.exe` subcommands |
| `@roost/shared/windows-relocation` | Windows relocation command + journal shapes |
| `@roost/shared/log` | `log.{debug,info,warn,error}` — the coord/worker sink |
| `@roost/shared/diag` | `diag()` / `signal()` — opt-in firehose, always-on Tier-1 |
| `@roost/shared/trace` | `newTraceId()` + `TRACE_HEADER` |
| `@roost/shared/json` | `safeJsonParse` for rows that may be half-written |
| `@roost/shared/viewport` | viewer-claim TTL / grace / reap timings both ends must agree on |
| `@roost/shared/wterm-wasm` | patched wasm path + its committed sha256 |
| `@roost/shared/wterm-core-factory` | headless `TerminalCore` factory |
| `@roost/shared/install-scripts` | embedded `install.sh` text (generated) |

`src/service-health-protocol.ts` has no subpath on purpose: it is internal and
reached through `@roost/shared/service-health`, which re-exports it.

## Adding a wire field

1. Edit the `.proto` under `proto/roost/v1/` (`wire.proto`, `coordinator.proto`,
   `sync.proto`, `events.proto`, `cell.proto`, `worker_transport.proto`).
2. `bun run --filter='@roost/shared' proto:gen` (`buf generate`; config in
   `buf.gen.yaml` + `proto/buf.yaml`). Output lands in `src/gen/roost/v1/`, one
   `_pb.ts` per proto; generated files are never hand-edited and are excluded
   from the line-cap lint.
3. Update the Zod schema in `src/wire/` and its adapter in the same pass.
4. Every consumer typechecks against the regenerated code.

The `JsonEvent` Sync fallback remains for payloads without a typed frame and
for deployed-client compatibility; do not retire it without migrating both
producers and consumers.

## Module map

- **Wire (Zod)** — public barrel `src/wire/index.ts`; owners
  `src/wire/brand.ts`, `src/wire/worker.ts`, `src/wire/session.ts`,
  `src/wire/agent-status.ts`, `src/wire/event.ts` (`foldEvent`, consumed by BOTH
  projectors), `src/wire/control.ts`, `src/wire/coord-worker.ts`,
  `src/wire/sync-ws.ts`, `src/wire/headers.ts`, `src/wire/workspace.ts`,
  `src/wire/task.ts`, `src/wire/mcp.ts`, plus the `*-proto.ts` adapters.
- **Terminal input** — `src/terminal-input.ts` is the single encoder and limit
  owner shared by the browser composer and the worker's guarded prompt path.
  It normalizes every newline spelling to CR and, when bracketed paste is
  active, strips ESC from the text and wraps it; `CR_BYTES` supplies submit.
- **Portable layout document** — `src/layout-document.ts` owns the browser-safe
  v1 pane tree, leaf/slot session bindings, strict parser, and shared ratio
  bounds; `src/layout-document-preflight.ts` rejects excessive identifiers,
  depth, nodes, slots, and bindings before recursion; and
  `src/layout-document-proto.ts` maps the bounded graph to/from protobuf.
- **Agent conversation recovery** — `src/agent-conversation-reference.ts` owns
  the private versioned OMP reference, UTF-8/envelope limits, and
  `client_seq`-ordered recovery fold; `src/agent-conversation-reference-proto.ts`
  maps the reference and worker-only recovery row to/from protobuf.
- **UI state resource contract** — `src/ui-state.ts` owns allocation-free UTF-8
  measurement and the tab/report/cardinality limits used by Sync admission,
  coordinator report/live-target owners, and the CLI human-output formatter.
- **Terminal search** — `src/terminal-search.ts` owns query/row/match/preview
  limits, exclusive-cursor result validation, and Unicode code-point
  counting/truncation shared by every search hop.
- **Terminal cell model** — public barrel `src/cell/index.ts`;
  `src/cell/types.ts`, `src/cell/grid-to-cells.ts`, `src/cell/diff-grid.ts`,
  `src/cell/emitter.ts`, `src/cell/cell-proto.ts`, and snapshot owners
  `src/cell/frame-chunks.ts`, `src/cell/frame-chunk-validation.ts`,
  `src/cell/frame-chunk-assembler.ts`.
- **Config** — `src/coord-config-schema.ts` owns the declarative `CoordConfig`;
  `src/config.ts` owns environment normalization, secret resolution, cross-field
  policy, and the public re-export.
- **Platform + paths** — `src/platform.ts`, `src/paths.ts`, `src/native-path.ts`,
  `src/tenant-route.ts`, `src/tailnet.ts`, `src/durability.ts`,
  `src/local-endpoint.ts`, `src/service-health.ts`,
  `src/service-health-protocol.ts`, `src/build-identity.ts`,
  `src/machine-join-command.ts`, `src/shell-quote.ts`.
- **Observability** — `src/log.ts`, `src/diag.ts`, `src/trace.ts`, `src/json.ts`.
- **Identity + timing** — `src/fingerprint.ts`, `src/native-credentials.ts`,
  `src/jwt-base.ts`, `src/viewport.ts`, `src/retry.ts`.
- **Email** — `src/email-client.ts`, `src/email-payload.ts`.
- **Native / Windows** — `src/windows-helper.ts`, `src/windows-relocation.ts`.
- **WASM** — `src/wterm-core-factory.ts`, `src/wterm-wasm.ts`, `wasm/`.
- **Generated** — `src/gen/roost/v1/`, `src/install-scripts.generated.ts`,
  `src/wterm-wasm-embed.generated.ts`.

## Invariants

- **Conversation references are opaque private recovery state.**
  `AgentConversationReferenceV1` admits only schema version 1, agent `omp`,
  kind `id|path`, and a nonempty, well-formed, NUL-free value of at most 4,096
  UTF-8 bytes;
  its complete durable event is at most 8 KiB. Worker `client_seq`, not
  volatile agent status, orders set/replace/clear. The public Session fold is
  an explicit no-op, and the value never belongs in public Session, browser
  Sync, CLI session output, logs, or audit data.
- **Layout documents are strict, total, bounded graphs.** `schema_version` is
  exactly 1; every recursive object rejects unknown fields; split ratios are
  finite and within inclusive `0.1..0.9`; leaf and slot keys are nonempty and
  globally unique. Focus and selection must reference the owning tree,
  selection is null exactly for empty leaves, every slot has exactly one
  binding, and a session can be bound only once. An iterative preflight runs
  before recursive JSON or protobuf conversion and caps identifiers at 256
  UTF-8 bytes, depth at 32, nodes at 255, and slots/bindings at 512.
- **Portable JSON and protobuf share one validator.** Both directions through
  `src/layout-document-proto.ts` call `parseLayoutDocumentV1`; recursive oneofs
  cannot create a second acceptance policy. `UiReportStateRequest` carries the
  optional typed document, while retired runtime-layout JSON, pane IDs, visible
  session IDs, and unknown protobuf fields are rebuilt away at admission.
- **The UI acknowledgement fence is explicit on the wire.** The eight
  fire-and-forget `UiCommand` variants retain `UiDispatch` publication counts;
  `apply_layout` is admitted only by `UiApplyLayout`, whose request pins a
  nonempty browser fingerprint/tab tuple. Its command frame names that exact
  tab's socket and correlation, and a browser result can claim only `APPLIED`
  or `REJECTED`; tuple absence, socket close/replacement, or acknowledgement
  timeout resolves the RPC as `TARGET_GONE` without retry.

- **Terminal-search limits reject rather than clamp.** Queries may be empty
  and are capped at 256 Unicode code points, caller-generated cancellation IDs
  at 64 characters, pages at 4,096 complete rows, chains at 32 pages, results
  at 256 matches, previews at 512 Unicode code points, and the outer worker RPC
  at 8,000 ms.
  Worker JSON row indices stay nonnegative safe integers until the coordinator
  converts them to proto `uint64`; `before_row` and `next_before_row` are
  exclusive cursors; only `row_limit` returns a continuation.
  Dashboard-global pages separately cap enumeration at 32 sessions, scan 2,048
  rows per session, return 256 matches total within 5,000 ms, and retain at
  most four opaque cursors per device for 60,000 ms.
  Caller-requested global session/row/match limits are normalized down to those
  caps before cursor binding; worker-bound limits reject any out-of-range value.

- **Agent-status identity is an all-or-none fencing triple.** New statuses carry
  `status_epoch`, `occupant_id`, and worker-assigned `source` together; only
  legacy statuses may omit all three, and partial triples are invalid. The UUIDs
  are equality tokens, never ordering keys, and no process PID crosses a public
  wire. `promptable` exists only in the coordinator read projection and is true
  only for an identified integration source.

- **Agent-status waits carry exact identity.** `AgentStatusWait` names one
  `status_epoch` and `occupant_id`, a non-empty unique desired-state list, an
  optional exclusive revision fence, and a mandatory timeout no longer than
  five minutes. Its terminal outcomes are `matched`, `timed_out`,
  `occupant_changed`, and `session_closed`; transport cancellation is an RPC
  error, not another outcome.

- **Agent prompts carry a complete status fence.** `SessionsPrompt` in
  `proto/roost/v1/coordinator.proto` requires session, status epoch, occupant,
  safe-`uint64` revision, and nonempty text of at most 16,384 UTF-8 bytes.
  Wait configuration is either wholly absent or a nonempty unique list drawn
  from `idle|working|blocked` plus a timeout in `1..300000` ms. Its response
  separates `accepted|rejected|ambiguous` input from optional
  `matched|timed_out|occupant_changed|session_closed` wait outcome and carries
  only a reason of at most 200 characters and `written_bytes` of at most
  16,397, never the text.

- **Text encoding does not change raw input.** `DAgentPrompt` is the dedicated
  `CoordWorkerDown` oneof tag 16. It carries `request_id`, `session_id`,
  `input_seq`, exact status epoch/occupant/revision, original `text`, and
  relative `budget_ms`. The worker's guarded path calls
  `src/terminal-input.ts` and appends CR; `SessionsInput` and `DInputRequest`
  remain caller-supplied bytes with no normalization, paste wrapper, implicit
  Enter, or retry.

- **`src/fingerprint.ts` is the only pubkey fingerprint.** Hex SHA-256 of a raw
  32-byte ed25519 pubkey, and all three ends of the protocol must agree
  byte-for-byte forever — it is the JWT `kid`, the authorized-keys match, and the
  pairing identity. There were three hand-maintained copies with nothing tying
  them together: `apps/coord/src/jwt.ts`, `apps/worker/src/jwt.ts`, and
  `apps/web/src/auth/web-key.ts`. It uses `crypto.subtle`, not `node:crypto`,
  because that is the only implementation available in every runtime that
  computes the value — the browser included.
- **`src/native-path.ts` imports zero Node builtins, deliberately.** It is in the
  browser bundle graph (`apps/web/src/lib/nativePath.ts` imports it). Worker-side
  path handling lives separately in `apps/worker/src/util/path.ts` because it
  needs `node:path`/`node:fs`/`node:os` **and** because its POSIX contract
  differs: the worker passes POSIX values through byte-for-byte where shared
  validates and throws on empty/NUL/root-escaping input. They share a function
  name, not a behavior. Do not merge them.
- **The WASM artifact stays beside its loader.** `src/wterm-core-factory.ts`,
  `src/wterm-wasm.ts`, and `wasm/wterm-roost.wasm` +
  `wasm/wterm-roost.wasm.sha256` live here even though only the worker imports
  them: `src/wterm-core-factory.ts` states it is server-side only and refuses to
  load bytes that do not hash to the committed sidecar (no stock-WASM fallback —
  stock caps scrollback at 1k lines where Roost renders 10k, so a silent fallback
  truncates exactly the sessions that need history). CI's `wterm-wasm` job and
  `tests/wterm-core-load.test.ts` gate on the artifact and its checksum being
  co-located, and `apps/roost-cli/src/deploy.ts` rsyncs `apps/shared/` to every
  worker host, so the paths must resolve relative to this module.
- **`src/local-endpoint.ts` stays here.** Two shared modules import it —
  `src/service-health.ts` and `src/service-health-protocol.ts`. Moving it to the
  worker would invert the dependency.
- **`WorkerConfig` is not here.** It lives with its loader in
  `apps/worker/src/config.ts`. `src/config.ts` holds coord config only.

## Test

`bun test apps/shared/tests/` runs the recursive `**/*.test.ts` suites.
`tests/trace-oracle.ts` is the differential VT trace oracle that gates
the pinned WASM; it is a helper, not a spec, and is driven by
`tests/core-trace-oracle.test.ts`.
`tests/coordinator-transfer-retirement.test.ts` pins the absence of
cross-worker RPC/worker-wire variants while preserving attachment RPCs.
