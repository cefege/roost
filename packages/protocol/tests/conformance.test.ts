// Protocol conformance vectors exercise the canonical session fold and cell-chunk assembler.
// Each JSON file is loaded independently so a new vector becomes a named Bun test.
// Reference outcomes were generated from foldAll and chunkCellGridFrame in the contract package.

import { readdirSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { expect, test } from "bun:test";
import { fromJson, type JsonValue } from "@bufbuild/protobuf";
import {
  CellGridChunkAssembler,
  CellGridChunkError,
} from "../src/cell/index.ts";
import { PbCellGridChunkSchema } from "@roost/protocol/proto/cell_pb";
import { SessionEvent, foldAll, type Session } from "../src/wire/index.ts";

const VECTOR_ROOT = resolve(import.meta.dir, "../../../protocol/conformance");

interface SessionFoldVector {
  name: string;
  events: unknown[];
  expect: { sessions: Session[] };
}

interface CellChunkVector {
  name: string;
  chunks: JsonValue[];
  expect: string[];
}

function loadVectors<T>(directory: string): T[] {
  return readdirSync(join(VECTOR_ROOT, directory))
    .filter((file) => file.endsWith(".json"))
    .sort()
    .map((file) => JSON.parse(
      readFileSync(join(VECTOR_ROOT, directory, file), "utf8"),
    ) as T);
}

for (const vector of loadVectors<SessionFoldVector>("session-fold")) {
  test(vector.name, () => {
    const events = vector.events.map((event) => SessionEvent.parse(event));
    const sessions = [...foldAll(events).values()]
      .sort((left, right) => left.id.localeCompare(right.id));
    expect(sessions).toEqual(vector.expect.sessions);
  });
}

for (const vector of loadVectors<CellChunkVector>("cell-chunks")) {
  test(vector.name, () => {
    const assembler = new CellGridChunkAssembler();
    const outcomes = vector.chunks.map((chunkJson) => {
      const chunk = fromJson(PbCellGridChunkSchema, chunkJson);
      try {
        return assembler.push(chunk, 0).kind;
      } catch (error) {
        if (!(error instanceof CellGridChunkError)) throw error;
        return `error:${error.code}`;
      }
    });
    expect(outcomes).toEqual(vector.expect);
  });
}
