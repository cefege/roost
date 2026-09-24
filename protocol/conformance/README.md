<!--
Protocol conformance vectors are JSON fixtures for the public fold and cell-chunk assembler.
Each file is an independent oracle case; packages/protocol/tests/conformance.test.ts creates one Bun test per file.
-->
# Conformance vectors

## Session fold

`session-fold/*.json` has this shape:

```json
{
  "name": "opened",
  "events": [],
  "expect": { "sessions": [] }
}
```

`events` are JSON values accepted by `SessionEvent.parse`. `expect.sessions` is the
id-sorted array produced by `foldAll`; the runner compares it with `toEqual`.

## Cell chunks

`cell-chunks/*.json` has this shape:

```json
{
  "name": "single-complete",
  "chunks": [],
  "expect": []
}
```

`chunks` are canonical protobuf JSON values for `PbCellGridChunk`. Each expected
outcome is `pending`, `complete`, or `error:<CellGridChunkErrorCode>`, in input order.
The runner decodes each chunk with `fromJson`, pushes it through
`CellGridChunkAssembler`, and records the result or error code.

## Adding a vector

Add one JSON file, keep its name unique within its directory, and update the
throwaway generator when the fixture is derived from an implementation oracle.
Do not add tests that merely restate the JSON; the runner must exercise the
canonical parser, fold, decoder, or assembler.
