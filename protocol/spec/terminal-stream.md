<!-- Terminal-stream contract: authoritative replicas, full-before-delta repair, geometry, and chunk assembly. -->
<!-- Protocol meaning is fixed by protocol/proto/roost/v1/{cell,sync,local_terminal}.proto. -->
<!-- Cell-chunk conformance vectors live in protocol/conformance/cell-chunks/. -->

# Terminal stream

## Purpose

The worker owns the authoritative terminal core. Sync, loopback, and WebRTC carry the same cell model; a recipient installs a complete full before exact-successor deltas. PTY bytes never enter a browser, and renderer detach or carrier change cannot replace the session replica with a partial baseline.

## Messages

| Message | File | Meaning |
| --- | --- | --- |
| `PbCellSpan`, `PbCellRow` | `protocol/proto/roost/v1/cell.proto:9-47` | Styled cell runs with explicit column occupancy and indexed rows. |
| `PbCellGridFrame` | `protocol/proto/roost/v1/cell.proto:49-92` | Authoritative full/delta, cursor, modes, stream ID, epoch, sequence, viewport, and scrollback. |
| `PbCellGridChunk` | `protocol/proto/roost/v1/cell.proto:95-105` | Bounded whole-row part of one authoritative full. |
| `FirehoseFrame.cell_grid`, `cell_grid_chunk` | `protocol/proto/roost/v1/sync.proto:274-277` | Sync terminal delivery. |
| `TerminalViewCommand`, `TerminalResyncCommand`, `TerminalViewStateFrame` | `protocol/proto/roost/v1/sync.proto:145-188` | Socket-bound membership/geometry intent, repair request, and generation-matched result. |
| `LocalTerminalServerFrame.cell_grid`, `cell_grid_chunk` | `protocol/proto/roost/v1/local_terminal.proto:76-89` | Same cell frames on loopback/WebRTC. |

## State machine

1. Exactly one membership authority owns each session: worker `TerminalViewOwner` when advertised, otherwise coordinator `TerminalViewHub`. It computes independent-axis minimum geometry from active views. A first view, effective-size change, last-view disable/re-enable, worker replacement, or unavailable retry mints a new stream ID.
2. A visible pane republishes its view at the heartbeat interval and requires a generation-matched acknowledgement before the lease expires. Explicit hide, authorization loss, or durable close removes the view immediately. A dropped transport parks it: the record remains reclaimable for the lease, but stops constraining geometry after the shorter park grace.
3. The worker's `wterm` core is resized only at the keeper's ordered boundary. Acknowledged resize is the synchronization point; the existing core is resized synchronously before later `PtyOut` parsing. Resize invalidates the cell-emission epoch and forces a new full. No active views retain the last PTY geometry/core but gate emission.
4. Every stream generation begins with one complete authoritative full. A delta is accepted only when stream ID, grid epoch, dimensions, and `base_seq` match and `seq` is the exact successor. Any gap invalidates the cursor and latches one snapshot request; status frames and partial chunks never establish a baseline.
5. Full repair atomically replaces the canonical recipient replica. The coordinator maintains a Sync replica; the browser maintains one canonical replica and folds a direct candidate separately until promotion. Renderers keep their last complete DOM until replacement is ready.
6. View-state and cell frames share the per-session terminal lane. Session events commit before the authenticated binding/publication that announces the channel, so the first cell cannot overtake its opened/state predecessor.
7. `CellGridChunkAssembler` accumulates contiguous chunks, rejects repeated or missing viewport rows, and publishes only after all declared parts and rows validate. Sync materializes one next chunk only when queue/ACK room exists; direct ports own independent queues.

## Limits

| Constant | Value | TypeScript source |
| --- | ---: | --- |
| `CELL_GRID_PART_MAX_BYTES` | `1 MiB` | `packages/protocol/src/cell/frame-chunk-validation.ts:19` |
| `CELL_GRID_SNAPSHOT_MAX_BYTES` | `64 MiB` | `packages/protocol/src/cell/frame-chunk-validation.ts:24` |
| `CELL_GRID_SNAPSHOT_MAX_CHUNKS` | `256` | `packages/protocol/src/cell/frame-chunk-validation.ts:25` |
| `CELL_GRID_SNAPSHOT_MAX_ROWS` | `TERMINAL_MAX_ROWS = 256` | `packages/protocol/src/cell/frame-chunk-validation.ts:26`; `packages/protocol/src/viewport.ts:7` |
| `CELL_GRID_SNAPSHOT_MAX_SPANS` | `65,536` | `packages/protocol/src/cell/frame-chunk-validation.ts:27` |
| `CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS` | `1,024` | `packages/protocol/src/cell/frame-chunk-validation.ts:28` |
| `CELL_GRID_CHUNK_STALL_MS` | `10,000 ms` | `packages/protocol/src/cell/frame-chunk-validation.ts:29` |
| `TERMINAL_MAX_COLS` / `TERMINAL_MAX_ROWS` | `256 / 256` | `packages/protocol/src/viewport.ts:6-7` |
| `TERMINAL_VIEW_HEARTBEAT_MS` | `5,000 ms` | `packages/protocol/src/viewport.ts:10` |
| `TERMINAL_VIEW_LEASE_MS` | `15,000 ms` | `packages/protocol/src/viewport.ts:9` |
| `TERMINAL_VIEW_PARK_GRACE_MS` | `2,000 ms` | `packages/protocol/src/viewport.ts:16` |
| `TERMINAL_VIEW_SWEEP_MS` | `1,000 ms` | `packages/protocol/src/viewport.ts:11` |
| `TERMINAL_SOCKET_VIEW_CAP` | `64` | `packages/protocol/src/viewport.ts:20` |

## Errors

`CellGridChunkErrorCode` is the complete assembler error vocabulary:

`invalid-snapshot-id`, `invalid-stream-id`, `missing-part`, `invalid-full`, `invalid-geometry`, `invalid-sequence`, `chunk-count`, `chunk-index`, `chunk-order`, `chunk-size`, `snapshot-size`, `snapshot-stalled`, `metadata-mismatch`, `row-index`, `duplicate-row`, `missing-row`, `span-limit`, `link-limit`, `link-conflict`, `single-row-oversize` (`packages/protocol/src/cell/frame-chunk-validation.ts:31-51`).

Every viewport row `0..rows-1` must occur exactly once. A chunk must match snapshot ID, declared count, contiguous index, stream/generation metadata, and every scalar checked by `hasSameSnapshotMetadata`; duplicate rows are invalid even when byte-identical. Link keys may repeat only with the same URI.

## Reference implementation

- Cell schemas/adapters: `packages/protocol/src/cell/`, `packages/protocol/src/cell/frame-chunks.ts`
- Chunk validation/assembly: `packages/protocol/src/cell/frame-chunk-validation.ts`, `packages/protocol/src/cell/frame-chunk-assembler.ts`
- View authority: `packages/protocol/src/terminal-view/`, `apps/worker/src/terminal-view-owner.ts`, `apps/coord/src/terminal/view/terminal-view-hub.ts`
- Worker stream lifecycle: `apps/worker/src/terminal-view-owner-streams.ts`, `apps/coord/src/terminal/screen/terminal-stream-dispatcher.ts`
- Browser replica/promotion: `apps/web/src/store/terminal-stream.ts`, `apps/web/src/client/terminal-stream/terminal-stream-frame-fold.ts`, `apps/web/src/store/terminal-stream-promotion.ts`
- Conformance: `protocol/conformance/cell-chunks/`
