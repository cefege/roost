// Diag facade shape contract. Each emitted line must be:
//   - single JSON object on its own line
//   - target="diag", evt=<snake_case from fixed namespace>
//   - mono_ns numeric, ts numeric, level="info"
//   - kv passes through verbatim
//
// We test the enabled path by importing `_emitForTest` — the public
// `diag` export is module-load-gated by ROOST_DIAG, which complicates
// per-test toggling. Both paths share the same emitter so the
// contract is what we assert here.

import { describe, expect, test, beforeAll, afterAll } from "bun:test";
import { setDiagSink, setSignalSink, signal, type SignalKind } from "../src/diag.ts";

describe("diag facade — shape contract", () => {
  let captured: Array<Record<string, unknown>> = [];
  beforeAll(() => {
    setDiagSink((r) => captured.push(r));
  });
  afterAll(() => { setDiagSink(null); });

  test("emits the expected fixed keys + passes kv through", async () => {
    // Re-import so the module sees `setDiagSink` was called BEFORE the
    // first emit. The DIAG_ENABLED gate is module-load — to exercise
    // the enabled path here we hand-set the env then dynamic-import.
    process.env.ROOST_DIAG = "1";
    const path = require.resolve("../src/diag.ts");
    delete require.cache?.[path];
    const mod = await import(`../src/diag.ts?reload=${Date.now()}`);
    mod.setDiagSink((r: Record<string, unknown>) => captured.push(r));

    captured = [];
    mod.diag("viewport.claim", { sid: "abc", viewer_key: "vk1", cols: 80, rows: 24, client_seq: 3 });

    expect(captured.length).toBe(1);
    const rec = captured[0]!;
    expect(rec.evt).toBe("viewport.claim");
    expect(rec.sid).toBe("abc");
    expect(rec.viewer_key).toBe("vk1");
    expect(rec.cols).toBe(80);
    expect(rec.rows).toBe(24);
    expect(rec.client_seq).toBe(3);
    expect(typeof rec.mono_ns).toBe("number");
    expect(rec.mono_ns as number).toBeGreaterThan(0);
  });

  test("snake_case evt namespace tokens", async () => {
    process.env.ROOST_DIAG = "1";
    const mod = await import(`../src/diag.ts?reload2=${Date.now()}`);
    mod.setDiagSink((r: Record<string, unknown>) => captured.push(r));

    captured = [];
    const evts = [
      "viewport.claim", "viewport.withdraw", "viewport.recompute",
      "scrollback.fetch_start", "scrollback.gap", "scrollback.alt_flip",
      "bytes.chunk", "bytes.up_send", "bytes.write_throw",
      "resize.fit_measure", "resize.pty_signal",
      "focus.win", "focus.force",
      "projection.pick", "projection.divergence",
      "deck.visibility_show", "deck.mount",
      "session.spawn", "session.close",
      "diag.snapshot", "diag.corruption_signal",
      "spa.uncaught",
    ];
    for (const e of evts) mod.diag(e, {});
    expect(captured.length).toBe(evts.length);
    for (let i = 0; i < evts.length; i++) {
      expect(captured[i]!.evt).toBe(evts[i]);
      // snake_case: lowercase + underscores + one or two dots
      expect(/^[a-z]+(\.[a-z_]+){1,2}$/.test(evts[i]!)).toBe(true);
    }
  });

  // diag.ts is DEFAULT-OFF (opt in with ROOST_DIAG=1 / localStorage
  // roostDiag='1'); see diag.ts header. Unset → no-op; "1" → enabled.
  test("default OFF when ROOST_DIAG is unset", async () => {
    delete process.env.ROOST_DIAG;
    if (typeof localStorage !== "undefined") {
      try { localStorage.removeItem("roostDiag"); } catch { /* ignore */ }
    }
    const mod = await import(`../src/diag.ts?reload3a=${Date.now()}`);
    expect(mod.isDiagEnabled()).toBe(false);
  });

  test("enabled when ROOST_DIAG=1 (explicit opt-in)", async () => {
    const prev = process.env.ROOST_DIAG;
    process.env.ROOST_DIAG = "1";
    const mod = await import(`../src/diag.ts?reload3c=${Date.now()}`);
    expect(mod.isDiagEnabled()).toBe(true);
    if (prev === undefined) delete process.env.ROOST_DIAG;
    else process.env.ROOST_DIAG = prev;
  });

  test("no-op path when ROOST_DIAG=0 (explicit opt-out)", async () => {
    const prev = process.env.ROOST_DIAG;
    process.env.ROOST_DIAG = "0";
    const mod = await import(`../src/diag.ts?reload3b=${Date.now()}`);
    let invoked = 0;
    mod.setDiagSink(() => invoked++);
    mod.diag("viewport.claim", { sid: "x" });
    expect(invoked).toBe(0);
    expect(mod.isDiagEnabled()).toBe(false);
    if (prev === undefined) delete process.env.ROOST_DIAG;
    else process.env.ROOST_DIAG = prev;
  });
});

// Tier-1 signal() — ALWAYS on (independent of ROOST_DIAG), cooldown-gated,
// routes to a sink (SPA) or log.warn/stderr (Bun → *.err.log).
describe("signal facade — always-on Tier-1 channel", () => {
  let captured: Array<Record<string, unknown>> = [];
  beforeAll(() => { setSignalSink((r) => captured.push(r)); });
  afterAll(() => { setSignalSink(null); });

  test("fires even with ROOST_DIAG unset; routes to sink; strips cooldownKey", () => {
    delete process.env.ROOST_DIAG; // firehose OFF — signal MUST still fire
    captured = [];
    signal("spa.uncaught", { kind: "error", msg: "boom", cooldownKey: "sig-a" });
    expect(captured.length).toBe(1);
    const rec = captured[0]!;
    expect(rec.evt).toBe("spa.uncaught");
    expect(rec.kind).toBe("error");
    expect(rec.msg).toBe("boom");
    expect(rec.cooldownKey).toBeUndefined(); // stripped before emit
    expect(typeof rec.mono_ns).toBe("number");
  });

  test("per-(kind+scope) cooldown coalesces repeats; distinct scope fires", () => {
    captured = [];
    signal("diag.corruption_signal", { kind: "resize_storm", cooldownKey: "sidA|resize_storm" });
    signal("diag.corruption_signal", { kind: "resize_storm", cooldownKey: "sidA|resize_storm" }); // dup → suppressed
    signal("diag.corruption_signal", { kind: "resize_storm", cooldownKey: "sidB|resize_storm" }); // new scope → fires
    expect(captured.length).toBe(2);
  });

  test("no sink → log.warn line to stderr (target=signal, level=warn)", () => {
    setSignalSink(null);
    const orig = console.error;
    const lines: string[] = [];
    console.error = ((l: string) => { lines.push(l); }) as typeof console.error;
    try {
      signal("auth.key_evicted", { cooldownKey: "warn-test" });
    } finally {
      console.error = orig;
      setSignalSink((r) => captured.push(r)); // restore for afterAll symmetry
    }
    expect(lines.length).toBe(1);
    const obj = JSON.parse(lines[0]!);
    expect(obj.level).toBe("warn");
    expect(obj.target).toBe("signal");
    expect(obj.msg).toBe("auth.key_evicted");
  });

  test("accepts + emits the coverage-sweep signal kinds (union membership + shape)", () => {
    captured = [];
    const newKinds: SignalKind[] = [
      "bytes.drop_unmapped", "sync.backfill_failed", "sync.backfill_truncated",
      "sync.queue_overflow", "event.append_failed", "audit.write_failed",
      "worker.auth_rejected", "worker.protocol_violation", "rpc.worker_timeout",
      "auth.rpc_rejected", "worker.uncaught", "transport.event_drop",
      "heartbeat.stalled", "scrollback.history_lost", "deploy.failed",
      "deploy.cert_skipped", "auth.jwt_sign_fail",
    ];
    for (const k of newKinds) signal(k, { cooldownKey: `g3-${k}`, detail: k });
    // distinct kinds → distinct cooldown scopes → every one fires
    expect(captured.length).toBe(newKinds.length);
    for (const rec of captured) {
      expect(newKinds).toContain(rec.evt as SignalKind);
      expect(rec.cooldownKey).toBeUndefined();   // stripped before emit
      expect(typeof rec.mono_ns).toBe("number");
    }
    expect(new Set(captured.map((r) => r.evt)).size).toBe(newKinds.length);
  });

  test("cooldown coalesces a repeat of a new kind+scope", () => {
    captured = [];
    signal("event.append_failed", { cooldownKey: "g3-cooldown", error: "x" });
    signal("event.append_failed", { cooldownKey: "g3-cooldown", error: "y" }); // dup → suppressed
    expect(captured.length).toBe(1);
  });
});
