// SPA diag sink fail-safety contract. The sink JSON-encodes kv before shipping
// it to coord, and terminal input emits a diagnostic on every keystroke — so a
// kv field JSON cannot express natively (proto uint64 arrives as bigint) or at
// all (a cycle, a hostile getter) must degrade the DIAGNOSTIC, never the
// product path that emitted it. Drives the real @roost/observability/diag facade
// through the real batcher; only the coord transport is substituted.

import { afterAll, afterEach, beforeEach, describe, expect, mock, test, vi } from "bun:test";

interface ShippedEntry { evt: string; kvJson: string; sid: string }

// The DIAG_ENABLED gate is module-load, so the env flag and the transport mock
// must both precede the first import of either module.
process.env.ROOST_DIAG = "1";
const shipped: ShippedEntry[] = [];
mock.module("../src/connect.ts", () => ({
  coordinatorRpcUrl: (path: string) => `http://coord.test${path}`,
  coordClient: {
    diagDebugLogBatch: async (request: { entries: ShippedEntry[] }) => {
      shipped.push(...request.entries);
    },
  },
}));
const { diag, setDiagSink } = await import("@roost/observability/diag");
const spaDiag = await import("../src/lib/diag.ts");
spaDiag.installSpaDiag();
// The gate and the sink are process-global; the module-load gate has already
// been read, so both are released before any sibling file runs.
delete process.env.ROOST_DIAG;

const FLUSH_INTERVAL_MS = 100;

function flushShipped(): ShippedEntry[] {
  vi.advanceTimersByTime(FLUSH_INTERVAL_MS);
  return shipped;
}

function kvOf(entry: ShippedEntry | undefined): Record<string, unknown> {
  return JSON.parse(entry?.kvJson ?? "{}") as Record<string, unknown>;
}

describe("SPA diag sink — unserializable payloads never reach the caller", () => {
  beforeEach(() => { vi.useFakeTimers(); shipped.length = 0; });
  afterEach(() => { vi.useRealTimers(); });
  afterAll(() => { setDiagSink(null); });

  test("ships a bigint field as its exact decimal string", () => {
    diag("bytes.up_send", { sid: "s1", dir: "up", input_seq: 9007199254740993n });

    const entries = flushShipped();
    expect(entries).toHaveLength(1);
    expect(entries[0]!.evt).toBe("bytes.up_send");
    // Beyond Number.MAX_SAFE_INTEGER: a number round-trip would report ...992.
    expect(kvOf(entries[0]).input_seq).toBe("9007199254740993");
    expect(kvOf(entries[0]).dir).toBe("up");
  });

  test("flags a cyclic payload as degraded and still ships the event", () => {
    const cyclic: Record<string, unknown> = { sid: "s2", reason: "wheel" };
    cyclic.self = cyclic;

    diag("cell.apply", cyclic);

    const entries = flushShipped();
    expect(entries).toHaveLength(1);
    expect(entries[0]!.evt).toBe("cell.apply");
    expect(entries[0]!.sid).toBe("s2");
    expect(kvOf(entries[0]).kv_unserializable).toBe(true);
  });

  test("drops a diagnostic the sink cannot record at all, then keeps shipping", () => {
    diag("bytes.up_send", { sid: "s3", ts_ms: "not-a-number" });
    diag("bytes.up_send", { sid: "s4", len: 3 });

    const entries = flushShipped();
    expect(entries.map((entry) => entry.sid)).toEqual(["s4"]);
  });
});
