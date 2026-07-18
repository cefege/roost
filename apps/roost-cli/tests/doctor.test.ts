// `roost doctor` digest classification contract. Feeds synthetic err.log
// JSON lines through classify() + renderDigest() (the pure core) so the
// signal-grouping, sub-kind rollup, infra split, cutoff filter, and
// exit-code semantics are covered without touching real log files.

import { describe, expect, test } from "bun:test";
import { newDigest, classify, renderDigest, type Digest } from "../src/doctor.ts";

const NOW = 1_781_900_000_000;
const cutoff = NOW - 24 * 3600e3;

function fold(lines: Array<Record<string, unknown>>): Digest {
  const d = newDigest();
  for (const l of lines) classify(d, l, cutoff, "coord");
  return d;
}

describe("doctor — classification + digest", () => {
  test("groups signals by kind, rolls up sub-kinds, counts sids", () => {
    const d = fold([
      { ts: NOW, level: "warn", target: "signal", msg: "diag.corruption_signal", kind: "resize_storm", sid: "s1", src: "spa" },
      { ts: NOW, level: "warn", target: "signal", msg: "diag.corruption_signal", kind: "resize_storm", sid: "s2", src: "spa" },
      { ts: NOW, level: "warn", target: "signal", msg: "diag.corruption_signal", kind: "suppress_imbalance", sid: "s1", src: "spa" },
      { ts: NOW, level: "warn", target: "signal", msg: "auth.relogin_401", sid: "s1", src: "spa" },
    ]);
    const corr = d.signals.get("diag.corruption_signal")!;
    expect(corr.count).toBe(3);
    expect(corr.sids.size).toBe(2);
    expect(corr.subKinds.get("resize_storm")).toBe(2);
    expect(corr.subKinds.get("suppress_imbalance")).toBe(1);
    expect(d.signals.get("auth.relogin_401")!.count).toBe(1);
  });

  test("infra warnings split out from signals + counted by target/msg", () => {
    const d = fold([
      { ts: NOW, level: "warn", target: "worker-service", msg: "reader_failed" },
      { ts: NOW, level: "warn", target: "worker-service", msg: "reader_failed" },
      { ts: NOW, level: "warn", target: "pending-rpcs", msg: "timeout" },
      { ts: NOW, level: "warn", target: "signal", msg: "spa.uncaught", src: "spa" },
    ]);
    expect(d.infra.get("worker-service / reader_failed")).toBe(2);
    expect(d.infra.get("pending-rpcs / timeout")).toBe(1);
    expect(d.signals.size).toBe(1); // signal not counted as infra
  });

  test("drops lines older than cutoff and non-warn/error levels", () => {
    const d = fold([
      { ts: cutoff - 1, level: "warn", target: "signal", msg: "spa.uncaught" }, // too old
      { ts: NOW, level: "info", target: "diag", msg: "viewport.claim" },        // firehose, ignored
      { ts: NOW, level: "warn", target: "signal", msg: "spa.uncaught", src: "spa" }, // kept
    ]);
    expect([...d.signals.values()].reduce((a, g) => a + g.count, 0)).toBe(1);
  });

  test("exit 1 when any signal present; exit 0 when only routine infra", () => {
    const withSignal = renderDigest(
      fold([{ ts: NOW, level: "warn", target: "signal", msg: "input.drop_burst", src: "spa" }]),
      "24h", cutoff, [],
    );
    expect(withSignal.exit).toBe(1);

    const infraOnly = renderDigest(
      fold([{ ts: NOW, level: "warn", target: "worker-service", msg: "reader_failed" }]),
      "24h", cutoff, [],
    );
    expect(infraOnly.exit).toBe(0);

    const errorLevel = renderDigest(
      fold([{ ts: NOW, level: "error", target: "coord", msg: "boom" }]),
      "24h", cutoff, [],
    );
    expect(errorLevel.exit).toBe(1); // error-level always trips
  });

  test("groups by evt even when a kv msg collides (spa.uncaught error text)", () => {
    const d = fold([
      { ts: NOW, level: "warn", target: "signal", evt: "spa.uncaught", msg: "Cannot read x of null", kind: "error", sid: "s1", src: "spa" },
      { ts: NOW, level: "warn", target: "signal", evt: "spa.uncaught", msg: "totally different error text", kind: "error", sid: "s2", src: "spa" },
    ]);
    expect(d.signals.size).toBe(1);                       // both grouped under evt
    expect(d.signals.get("spa.uncaught")!.count).toBe(2); // not split by msg text
  });

  test("renders the kind name + count into the digest text", () => {
    const { text } = renderDigest(
      fold([
        { ts: NOW, level: "warn", target: "signal", msg: "diag.corruption_signal", kind: "resize_storm", sid: "s1", src: "spa" },
        { ts: NOW, level: "warn", target: "signal", msg: "diag.corruption_signal", kind: "resize_storm", sid: "s1", src: "spa" },
      ]),
      "24h", cutoff, [],
    );
    expect(text).toContain("diag.corruption_signal");
    expect(text).toContain("resize_storm×");
  });
});

describe("doctor — coverage-sweep kinds", () => {
  test("classifies new kinds; ERROR_SIGNALS → 🔴, others → ⚠️; exit 1", () => {
    const d = fold([
      { ts: NOW, level: "warn", target: "signal", msg: "event.append_failed", src: "coord" },
      { ts: NOW, level: "warn", target: "signal", msg: "rpc.worker_timeout", src: "coord" },
      { ts: NOW, level: "warn", target: "signal", msg: "deploy.failed", src: "coord" },
      { ts: NOW, level: "warn", target: "signal", msg: "sync.queue_overflow", src: "coord" },
    ]);
    expect(d.signals.get("event.append_failed")!.count).toBe(1);
    expect(d.signals.get("rpc.worker_timeout")!.count).toBe(1);
    expect(d.signals.get("sync.queue_overflow")!.count).toBe(1);
    const { text, exit } = renderDigest(d, "24h", cutoff, []);
    expect(exit).toBe(1); // any signal → non-zero
    const line = (kind: string) => text.split("\n").find((l) => l.includes(kind)) ?? "";
    expect(line("event.append_failed")).toContain("🔴"); // ERROR_SIGNALS → red
    expect(line("deploy.failed")).toContain("🔴");        // ERROR_SIGNALS → red
    expect(line("rpc.worker_timeout")).toContain("⚠️");   // warn kind
    expect(line("sync.queue_overflow")).toContain("⚠️");  // warn kind
  });

  test("attributes a keeper-subprocess signal to src:keeper", () => {
    const d = newDigest();
    classify(d, { ts: NOW, level: "warn", target: "signal", msg: "keeper.died", kind: "adopted_socket_close" }, cutoff, "keeper");
    expect([...d.signals.get("keeper.died")!.srcs]).toContain("keeper");
    expect(renderDigest(d, "24h", cutoff, []).text).toContain("src:keeper");
  });
});
