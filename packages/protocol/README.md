<!-- AUDIENCE: claude -->
<!-- Browser-safe protocol implementation: subpath imports only; generated protobufs and contract vectors are checked in. -->
<!-- Normative state machines and limits live in protocol/spec; this file records package ownership and wire-field workflow. -->

# @roost/protocol

Portable wire schemas, event folding, terminal cell contracts, direct-peer framing, pairing/UI policy, and generated protobuf bindings. It may import `@roost/observability` and `@wterm/core` only as type input where the package boundary requires it; it must not import an application, Node host runtime, or browser framework.

## Import rule: subpath-only

`import { X } from "@roost/protocol"` does not resolve. The package has no `.` export and no root barrel. Import the explicit owner:

```ts
import { foldEvent } from "@roost/protocol/wire";
import { CellGridChunkAssembler } from "@roost/protocol/cell";
import { TerminalViewRegistry } from "@roost/protocol/terminal-view";
```

| Subpath | Supplies |
| --- | --- |
| `@roost/protocol/wire` and `@roost/protocol/wire/*` | Zod wire schemas, branded identities, event/control/coord-worker schemas, Sync headers/negotiation, and protobuf adapters. |
| `@roost/protocol/cell` and `@roost/protocol/cell/*` | Cell-grid types, full/delta conversion, wterm row reader, frame chunks, validation, assembly, and cell protobuf adapters. |
| `@roost/protocol/terminal-view` | Shared terminal membership, lease/park state, geometry minimization, and screen-port contract. |
| `@roost/protocol/terminal-peer`, `terminal-peer-sdp`, `terminal-peer-packets`, `terminal-input`, `terminal-metadata`, `terminal-search`, `terminal-capture`, `terminal-capture-validate`, `terminal-core-capacity`, `terminal-core-capacity-proto`, `attachment-transfer`, `attachment-transfer-packets`, `pairing`, `layout-document`, `layout-document-proto`, `ui-state`, `agent-conversation-reference`, `agent-conversation-reference-proto`, `fingerprint`, `json`, `retry`, `viewport`, `keeper-update`, `keeper-update-proto`, `host-identity-proto`, `coordinator-dial-url`, `local-ui-door`, and `fleet-update` | Direct-peer/attachment contracts, input/search/metadata limits, portable layout/UI policy, pairing constants, identity, recovery, and coordination policy. |
| `@roost/protocol/proto/*` | Generated `src/gen/roost/v1/*_pb.ts` bindings. Never hand-edit generated files. |

## Module map

One row per current owned source/test directory. `protocol/proto/` is the language-neutral input tree for generation; generated runtime code is under `packages/protocol/src/gen/`.

| Directory | Owns | Must not own |
| --- | --- | --- |
| `packages/protocol/src/` | Contract modules not grouped below: peer/attachment contracts, pairing, layout, UI state, identity, retry, viewport, search, input, metadata, keeper update, host identity, and coordination policy. | Bun/Node host operations, app state, or UI rendering. |
| `packages/protocol/src/wire/` | Zod schemas, branded identities, `foldEvent`/`foldAll`, control/coord-worker schemas, Sync WS constants, headers, and `*-proto.ts` adapters. | Generated protobufs, app state, or transport connections. |
| `packages/protocol/src/cell/` | Cell types, grid/full/delta conversion, wterm row reader, emitter, bounded snapshot chunks, assembler, and cell proto adapter. | Worker runtime, renderer, or raw VT parsing. |
| `packages/protocol/src/terminal-view/` | `TerminalViewRegistry` state machine, commands/operations, protocol types, and `ScreenPort`. | A particular coord/worker screen implementation. |
| `packages/protocol/src/gen/` | Generated protobuf tree root; generated files only. | Handwritten contract policy or generated-file fixes. |
| `packages/protocol/src/gen/roost/` | Generated protobuf package namespace. | Source `.proto` edits. |
| `packages/protocol/src/gen/roost/v1/` | Generated `roost.v1` TypeScript bindings consumed by package subpath imports. | Manual edits; regenerate with `proto:gen`. |
| `packages/protocol/tests/` | Contract, fold, cell, peer, attachment, layout, identity, and conformance-vector tests. | Application integration tests or app state fixtures. |
| `protocol/proto/` | Language-neutral `.proto` inputs and Buf configuration. | TypeScript implementation logic. |

## Adding a wire field

1. Edit the relevant file under `protocol/proto/roost/v1/` and keep package `roost.v1` compatibility. Buf uses `breaking.use: FILE` in `protocol/proto/buf.yaml`.
2. Run `bun run --filter='@roost/protocol' proto:gen`; generated output lands in `packages/protocol/src/gen/roost/v1/`.
3. Update the owning Zod schema and its proto adapter in the same change; do not add a second validator.
4. Update every producer/consumer and add or update a language-neutral vector under `protocol/conformance/` when fold or chunk behavior changes. The format is defined in [`protocol/conformance/README.md`](../../protocol/conformance/README.md).
5. Run the package conformance suite and the relevant app tests. Normative behavior belongs in the matching [`protocol/spec`](../../protocol/spec/) files, not only in TypeScript.

- `local_terminal.proto` is the carrier-independent terminal message schema. WebRTC adds the mandatory bounded outer packet header in `terminal-peer-packets.ts`; loopback keeps its WebSocket frame boundary. Neither carrier invents a second terminal payload schema.
- `terminal-view` owns membership/lease/park state and geometry minimization. Coord and worker inject their own `ScreenPort`; the registry does not own a transport.
- Cell snapshots and deltas are bounded and ordered. `frame-chunk-validation.ts` is the single chunk admission/assembler contract; errors use `CellGridChunkErrorCode` and are covered by `protocol/conformance/cell-chunks/`.
- Event folding is one implementation in `wire/event.ts`; coordinator and browser projections consume it. Session fold vectors live in `protocol/conformance/session-fold/`.
- `fingerprint.ts` is the only pubkey fingerprint. `pairing.ts` is the browser-safe ceremony input owner. Conversation references and layout documents remain bounded, private or strictly validated as specified by their protocol specs.
- STUN is coordinator configuration/discovery only; it never carries terminal cells, grants, or Sync payloads. TURN and browser-supplied ICE policy are not part of this package.
- Direct attachment and terminal packet queues retain bounded accepted-once buffers and reject over-limit input rather than silently clamping it.
- Generated protobufs are never hand-edited. `JsonEvent` compatibility is retired only after producers and consumers migrate together.

The authoritative state machines, limits, and error vocabulary are in [`protocol/spec/session-events.md`](../spec/session-events.md), [`protocol/spec/sync.md`](../spec/sync.md), [`protocol/spec/terminal-stream.md`](../spec/terminal-stream.md), [`protocol/spec/direct-terminal.md`](../spec/direct-terminal.md), [`protocol/spec/attachments.md`](../spec/attachments.md), [`protocol/spec/auth-and-pairing.md`](../spec/auth-and-pairing.md), [`protocol/spec/agent-metadata.md`](../spec/agent-metadata.md), and [`protocol/spec/worker-link.md`](../spec/worker-link.md).

## Test

`bun test packages/protocol/tests/` runs the package suite. `packages/protocol/tests/conformance.test.ts` loads every JSON vector under `protocol/conformance/`.
