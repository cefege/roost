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

`bun test apps/shared/tests/` — 27 `**/*.test.ts` files, 218 registered
tests. `tests/trace-oracle.ts` is the differential VT trace oracle that gates
the pinned WASM; it is a helper, not a spec, and is driven by
`tests/core-trace-oracle.test.ts`.
`tests/coordinator-transfer-retirement.test.ts` pins the absence of
cross-worker RPC/worker-wire variants while preserving attachment RPCs.
