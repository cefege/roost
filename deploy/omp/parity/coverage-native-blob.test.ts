import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import type { InventoryEntry } from "./inventory-types";
import type { WireSchemaEvidence } from "./coverage-schema";
import { nativeContractProofs } from "./coverage-native";

const bytes = Buffer.from("native blob bytes");
const row = (sequence: number, verb: string, direction: "in" | "out", body: unknown) => ({ type: "wire", adapter: "browser", pid: 1, generation: "g", sequence, source: "s", line: 1, verb, direction, body });
const entry = { id: "verb:b", category: "verb", source: "s", line: 1, details: { exposure: "native-contract" } } as unknown as InventoryEntry;
const reader = { inspect: () => ({ facts: new Set(["verb:b"]), values: [] }) } as unknown as WireSchemaEvidence;

function proofs(queriedId: string) {
  const assertion = { name: "blob rendered", browser: { type: "dom", selector: "x", read: "count" }, expected: 1, actual: 1, passed: true };
  const contract = {
    entry: "verb:b", witness: "request", assertions: [assertion],
    request: { verb: "b", direction: "out", occurrence: "last" },
    response: { verb: "q", direction: "out", occurrence: "last", match: { "/q": "blobs" } },
  };
  const scenario = {
    id: "blob", covers: [], raw: [],
    wire: [row(1, "q", "out", { q: "blobs", ids: [queriedId] }), row(2, "b", "out", bytes.toString("base64"))],
    checkpoints: [{ name: "c", nativeContracts: [contract], probeCursors: { browser: { pid: 1, generation: "g", sequence: 2 } } }],
  };
  return nativeContractProofs(new Map([["verb:b", entry]]), scenario as never, reader);
}

describe("verb:b native contract", () => {
  test("is proven by the query that requested exactly those bytes", () => {
    expect(proofs(createHash("sha256").update(bytes).digest("hex")).map(proof => proof.entry)).toEqual(["verb:b"]);
  });
  test("never credits a native contract from a frame the ACP relay synthesized", () => {
    const relayed = { entry: "reply:blobs", witness: "request", assertions: [{ name: "n", browser: { type: "dom", selector: "x", read: "count" }, expected: 1, actual: 1, passed: true }],
      request: { verb: "r", direction: "in", occurrence: "last", match: { "/r": "blobs" } } };
    const scenario = { id: "relay", covers: [], raw: [], wire: [{ ...row(1, "r", "in", { r: "blobs", have: [] }), origin: "relay" }],
      checkpoints: [{ name: "c", nativeContracts: [relayed], probeCursors: { browser: { pid: 1, generation: "g", sequence: 1 } } }] };
    const relayEntry = { ...entry, id: "reply:blobs", category: "reply" } as unknown as InventoryEntry;
    expect(() => nativeContractProofs(new Map([["reply:blobs", relayEntry]]), scenario as never, reader)).toThrow("no actual correlated request/response");
  });
  test("rejects a blob whose hash was never queried", () => {
    expect(() => proofs("0".repeat(64))).toThrow("blob id its terminal query requested");
  });
});

describe("verb:q native contract", () => {
  const queryEntry = { id: "verb:q", category: "verb", source: "s", line: 1, details: { exposure: "native-contract" } } as unknown as InventoryEntry;
  const queryReader = { inspect: () => ({ facts: new Set(["verb:q"]), values: [] }) } as unknown as WireSchemaEvidence;
  const prove = (queriedId: string) => {
    const assertion = { name: "blob rendered", browser: { type: "dom", selector: "x", read: "count" }, expected: 1, actual: 1, passed: true };
    const contract = { entry: "verb:q", witness: "request", assertions: [assertion], request: { verb: "q", direction: "out", occurrence: "last", match: { "/q": "blobs" } }, response: { verb: "b", direction: "out", occurrence: "last" } };
    const scenario = { id: "q", covers: [], raw: [], wire: [row(1, "q", "out", { q: "blobs", ids: [queriedId] }), row(2, "b", "out", bytes.toString("base64"))],
      checkpoints: [{ name: "c", nativeContracts: [contract], probeCursors: { browser: { pid: 1, generation: "g", sequence: 2 } } }] };
    return nativeContractProofs(new Map([["verb:q", queryEntry]]), scenario as never, queryReader);
  };
  test("is proven by the inline transfer of a requested id", () => {
    expect(prove(createHash("sha256").update(bytes).digest("hex")).length).toBeGreaterThan(0);
  });
  test("is refused when the transferred bytes were not requested", () => {
    expect(() => prove("0".repeat(64))).toThrow("inline transfer");
  });
});
