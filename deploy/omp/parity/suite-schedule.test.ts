import { describe, expect, test } from "bun:test";
import type { ParityCase } from "./cases";
import { exclusivePorts, runScheduled, shardSelection, type ScheduledItem } from "./suite-schedule";

function builtin(id: string, requirements: Record<string, unknown>): ParityCase {
  return { kind: "builtin", scenario: { id, covers: [] }, requirements } as unknown as ParityCase;
}

// Each case finishes only when the test releases it, so ordering is driven by
// the test rather than by wall-clock delays.
function gatedCases() {
  const gates = new Map<string, PromiseWithResolvers<void>>();
  const running = new Set<string>();
  const started: string[] = [];
  const runOne = async ({ item }: ScheduledItem) => {
    const gate = Promise.withResolvers<void>();
    gates.set(item.scenario.id, gate);
    running.add(item.scenario.id);
    started.push(item.scenario.id);
    await gate.promise;
    running.delete(item.scenario.id);
    return item.scenario.id;
  };
  const release = async (id: string) => {
    gates.get(id)?.resolve();
    await new Promise(resolve => setImmediate(resolve));
  };
  return { running, started, runOne, release };
}

describe("parity case scheduling", () => {
  test("fixed listener ports come from declared port fields and port maps only", () => {
    expect(exclusivePorts(builtin("stats", { statsWorkflow: { ports: { dashboard: 3847, ignoredRequest: 3848, hostname: "127.0.0.1" } } }))).toEqual([3847, 3848]);
    expect(exclusivePorts(builtin("oauth", { services: [{ input: { auth: { oauthTransport: { callbackPort: 1455, deliveryBeforeCheckpoint: 1 } } } }] }))).toEqual([1455]);
    expect(exclusivePorts(builtin("plain", { lifecycle: { allowExit: false }, captures: [{ channel: "x" }], timeout: 3000 }))).toEqual([]);
  });

  test("cases sharing a fixed port never overlap while others fill free slots", async () => {
    const items: ScheduledItem[] = [
      builtin("a", { callbackPort: 1455 }), builtin("b", { callbackPort: 1455 }), builtin("c", {}), builtin("d", {}),
    ].map((item, index) => ({ index, item }));
    const cases = gatedCases();
    const done = runScheduled(items, 3, new AbortController().signal, cases.runOne, () => false);
    expect([...cases.running].sort()).toEqual(["a", "c", "d"]);
    await cases.release("c");
    expect([...cases.running].sort()).toEqual(["a", "d"]);
    await cases.release("a");
    expect([...cases.running].sort()).toEqual(["b", "d"]);
    await cases.release("b");
    await cases.release("d");
    expect(await done).toEqual(["a", "b", "c", "d"]);
  });

  test("a stopping result drains in-flight cases and starts no new ones", async () => {
    const items = ["a", "b", "c", "d"].map((id, index) => ({ index, item: builtin(id, {}) }));
    const cases = gatedCases();
    const done = runScheduled(items, 2, new AbortController().signal, cases.runOne, id => id === "a");
    await cases.release("a");
    expect(cases.started).toEqual(["a", "b"]);
    await cases.release("b");
    expect(await done).toEqual(["a", "b"]);
    expect(cases.started).toEqual(["a", "b"]);
  });

  test("shards partition the ordered selection exactly once", () => {
    const ids = Array.from({ length: 10 }, (_, index) => index);
    const slices = [1, 2, 3].map(index => shardSelection(ids, { index, count: 3 }));
    expect(slices.flat().sort((left, right) => left - right)).toEqual(ids);
    expect(slices.map(slice => slice.length)).toEqual([4, 3, 3]);
  });
});
