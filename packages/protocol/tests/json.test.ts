// Contract for the shared JSON boundary (`packages/protocol/src/json.ts`). Both
// directions exist to keep a malformed or unencodable value from throwing out
// of a path that had already committed (parse) or was only describing state
// (encode) — so what matters here is the value a consumer reads back, and that
// no input produces a throw.

import { describe, expect, test } from "bun:test";
import { safeJsonParse, safeJsonStringify } from "../src/json.ts";

const FALLBACK = '{"unencodable":true}';

describe("safeJsonStringify", () => {
  test("encodes bigint at every depth as its exact decimal string", () => {
    const encoded = safeJsonStringify({
      input_seq: 9007199254740993n,
      frame: { seq: 18446744073709551615n, full: true },
      stamps: [1n, { pty_out_ms: 1789450008631n }],
      len: 12,
    }, FALLBACK);

    // 9007199254740993 === 2^53 + 1: a Number() hop reports ...992 instead.
    expect(JSON.parse(encoded)).toEqual({
      input_seq: "9007199254740993",
      frame: { seq: "18446744073709551615", full: true },
      stamps: ["1", { pty_out_ms: "1789450008631" }],
      len: 12,
    });
  });

  test("falls back instead of throwing on a cyclic structure", () => {
    const cyclic: Record<string, unknown> = { sid: "s1" };
    cyclic.parent = { child: cyclic };

    expect(safeJsonStringify(cyclic, FALLBACK)).toBe(FALLBACK);
  });

  test("falls back instead of throwing when a property getter throws", () => {
    const hostile = {
      sid: "s2",
      get detail(): string { throw new Error("getter refused"); },
    };

    expect(safeJsonStringify(hostile, FALLBACK)).toBe(FALLBACK);
  });

  test("falls back for a value JSON has no representation for", () => {
    expect(safeJsonStringify(undefined, FALLBACK)).toBe(FALLBACK);
    expect(safeJsonStringify(() => "noop", FALLBACK)).toBe(FALLBACK);
  });
});

describe("safeJsonParse", () => {
  test("yields the consumer-schema fallback for absent or malformed input", () => {
    expect(safeJsonParse<number[]>(null, [], "session.ports")).toEqual([]);
    expect(safeJsonParse<number[]>("", [], "session.ports")).toEqual([]);
    expect(safeJsonParse<number[]>("[3000, 41", [], "session.ports")).toEqual([]);
    expect(safeJsonParse("{oops}", null, "host_metrics_json")).toBeNull();
  });

  test("parses well-formed input through unchanged", () => {
    expect(safeJsonParse<number[]>("[3000,5173]", [], "session.ports")).toEqual([3000, 5173]);
  });
});
