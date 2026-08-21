import { clone, create } from "@bufbuild/protobuf";
import { describe, expect, test } from "bun:test";
import {
  CELL_GRID_CHUNK_STALL_MS, CELL_GRID_PART_MAX_BYTES, CELL_GRID_SNAPSHOT_MAX_BYTES,
  CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS, CellGridChunkAssembler, CellGridChunkError,
  assertCellGridSnapshot, chunkCellGridFrame, decodeCellGridChunk, encodeCellGridChunk,
} from "../src/cell/index.ts";
import {
  PbCellGridChunkSchema, PbCellGridFrameSchema, PbCellRowSchema,
  type PbCellGridChunk, type PbCellGridFrame, type PbCellRow,
} from "../src/gen/roost/v1/cell_pb.ts";

const STREAM = "00000000-0000-4000-8000-000000000001";
const OTHER = "00000000-0000-4000-8000-000000000002";
const SNAP = "00000000-0000-4000-8000-000000000011";
const REPLACEMENT = "00000000-0000-4000-8000-000000000012";

function row(index: number, text = `r${index}`): PbCellRow {
  return { $typeName: "roost.v1.PbCellRow", index, spans: [{
    $typeName: "roost.v1.PbCellSpan", text, columns: 1, fg: 256, bg: 256, flags: 0,
  }] };
}
function frame(rows = 4, streamId = STREAM): PbCellGridFrame {
  return create(PbCellGridFrameSchema, {
    sessionId: "s", streamId, gridEpoch: "g", cols: 8, rows, full: true,
    viewportRows: Array.from({ length: rows }, (_, index) => row(index)),
    seq: 1n, baseSeq: 0n, sbBase: 0n, scrollbackTotal: 0n,
  });
}
function chunks(source: PbCellGridFrame, groups: PbCellRow[][], snapshotId = SNAP): PbCellGridChunk[] {
  return groups.map((rows, chunkIndex) => {
    const part = clone(PbCellGridFrameSchema, source);
    part.viewportRows = rows.map((row) => clone(PbCellRowSchema, row));
    return create(PbCellGridChunkSchema, { snapshotId, chunkIndex, chunkCount: groups.length, part });
  });
}
function code(action: () => unknown): string {
  try { action(); } catch (error) {
    expect(error).toBeInstanceOf(CellGridChunkError);
    return (error as CellGridChunkError).code;
  }
  throw new Error("expected rejection");
}

describe("cell grid chunk contract", () => {
  test("round-trips and installs atomically", () => {
    const source = frame(); const assembler = new CellGridChunkAssembler();
    let complete: PbCellGridFrame | undefined;
    for (const chunk of chunkCellGridFrame(source, SNAP)) {
      const result = assembler.push(decodeCellGridChunk(encodeCellGridChunk(chunk)), 1);
      if (result.kind === "complete") complete = result.frame;
    }
    expect(complete).toEqual(source);
  });
  test("round-trips an authoritative reconnect history bridge", () => {
    const source = frame();
    source.sbBase = 700n;
    source.scrollbackTotal = 703n;
    source.scrollbackRows = [row(700, "h700"), row(701, "h701"), row(702, "h702")];
    const assembler = new CellGridChunkAssembler();
    let complete: PbCellGridFrame | undefined;
    for (const chunk of chunkCellGridFrame(source, SNAP)) {
      const result = assembler.push(decodeCellGridChunk(encodeCellGridChunk(chunk)), 1);
      if (result.kind === "complete") complete = result.frame;
    }
    expect(complete).toEqual(source);
  });


  test("rejects missing/malformed and out-of-order chunks", () => {
    const missing = create(PbCellGridChunkSchema, { snapshotId: SNAP, chunkCount: 1 });
    expect(code(() => new CellGridChunkAssembler().push(missing))).toBe("missing-part");
    const source = frame(2); const values = chunks(source, [[source.viewportRows[0]!], [source.viewportRows[1]!]]);
    const malformed = chunks(source, [source.viewportRows])[0]!; malformed.part!.full = false;
    expect(code(() => new CellGridChunkAssembler().push(malformed))).toBe("invalid-full");
    expect(code(() => new CellGridChunkAssembler().push(values[1]!))).toBe("chunk-order");
    const duplicateIndex = new CellGridChunkAssembler(); duplicateIndex.push(values[0]!, 0);
    expect(code(() => duplicateIndex.push(values[0]!, 1))).toBe("chunk-order");
  });

  test("rejects duplicate and missing rows", () => {
    const source = frame(3);
    const duplicate = chunks(source, [[source.viewportRows[0]!, source.viewportRows[1]!], [source.viewportRows[1]!, source.viewportRows[2]!]]);
    const a = new CellGridChunkAssembler(); a.push(duplicate[0]!, 0);
    expect(code(() => a.push(duplicate[1]!, 1))).toBe("duplicate-row");
    const missing = chunks(source, [[source.viewportRows[0]!], [source.viewportRows[2]!]]);
    const b = new CellGridChunkAssembler(); b.push(missing[0]!, 0);
    expect(code(() => b.push(missing[1]!, 1))).toBe("missing-row");
  });

  test("rejects scalar, link, and replacement-stream mismatches", () => {
    const source = frame(2); const scalar = chunks(source, [[source.viewportRows[0]!], [source.viewportRows[1]!]]);
    scalar[1]!.part!.cursorCol = 1; const a = new CellGridChunkAssembler(); a.push(scalar[0]!, 0);
    expect(code(() => a.push(scalar[1]!, 1))).toBe("metadata-mismatch");
    source.viewportRows[0]!.spans[0]!.linkKey = "k"; source.viewportRows[0]!.spans[0]!.linkUri = "https://a";
    source.viewportRows[1]!.spans[0]!.linkKey = "k"; source.viewportRows[1]!.spans[0]!.linkUri = "https://b";
    const links = chunks(source, [[source.viewportRows[0]!], [source.viewportRows[1]!]]);
    const b = new CellGridChunkAssembler(); b.push(links[0]!, 0);
    expect(code(() => b.push(links[1]!, 1))).toBe("link-conflict");
    const clean = frame(2); const first = chunks(clean, [[clean.viewportRows[0]!], [clean.viewportRows[1]!]]);
    const other = frame(2, OTHER); const replacement = chunks(other, [[other.viewportRows[0]!], [other.viewportRows[1]!]], REPLACEMENT);
    const c = new CellGridChunkAssembler(); c.push(first[0]!, 0);
    expect(code(() => c.push(replacement[0]!, 1))).toBe("metadata-mismatch");
  });

  test("expires at the exact stall boundary", () => {
    const source = frame(2); const values = chunks(source, [[source.viewportRows[0]!], [source.viewportRows[1]!]]);
    const a = new CellGridChunkAssembler(); a.push(values[0]!, 1_000);
    expect(a.expire(1_000 + CELL_GRID_CHUNK_STALL_MS - 1)).toBe(false);
    expect(a.expire(1_000 + CELL_GRID_CHUNK_STALL_MS)).toBe(true);
    const b = new CellGridChunkAssembler(); b.push(values[0]!, 1_000);
    expect(code(() => b.push(values[1]!, 1_000 + CELL_GRID_CHUNK_STALL_MS))).toBe("snapshot-stalled");
  });

  test("rejects one-MiB part and sixty-four-MiB assembly overflow", () => {
    const one = frame(1); one.viewportRows[0]!.spans[0]!.text = "x".repeat(CELL_GRID_PART_MAX_BYTES);
    expect(code(() => new CellGridChunkAssembler().push(chunks(one, [one.viewportRows])[0]!))).toBe("chunk-size");
    expect(code(() => decodeCellGridChunk(new Uint8Array(CELL_GRID_PART_MAX_BYTES + 1)))).toBe("chunk-size");
    const source = frame(65); source.cols = 1;
    const payload = "x".repeat(CELL_GRID_PART_MAX_BYTES - 512);
    source.viewportRows = Array.from({ length: 65 }, (_, index) => row(index, payload));
    const values = chunks(source, source.viewportRows.map((value) => [value]));
    const assembler = new CellGridChunkAssembler(); let failure = "";
    for (let i = 0; i < values.length; i++) {
      try { assembler.push(values[i]!, i); } catch (error) { failure = (error as CellGridChunkError).code; break; }
    }
    expect(CELL_GRID_SNAPSHOT_MAX_BYTES).toBe(64 * 1024 * 1024);
    expect(failure).toBe("snapshot-size");
  });

  test("rejects more than 1,024 consistent link mappings", () => {
    const source = frame(5);
    source.cols = 256;
    source.viewportRows = Array.from({ length: 5 }, (_, rowIndex) => ({
      $typeName: "roost.v1.PbCellRow" as const,
      index: rowIndex,
      spans: Array.from({ length: rowIndex === 4 ? 1 : 256 }, (_, spanIndex) => ({
        $typeName: "roost.v1.PbCellSpan" as const,
        text: "x",
        columns: 1,
        fg: 256,
        bg: 256,
        flags: 0,
        linkKey: `key-${rowIndex}-${spanIndex}`,
        linkUri: `https://example.test/${rowIndex}/${spanIndex}`,
      })),
    }));
    expect(CELL_GRID_SNAPSHOT_MAX_LINK_MAPPINGS).toBe(1_024);
    expect(code(() => assertCellGridSnapshot(source))).toBe("link-limit");
  });
});
