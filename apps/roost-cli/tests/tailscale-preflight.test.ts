// ensureTailscale interactive-gate contract. Feeds scripted Tailscale states
// through the pure loop (injected resolve/sleep/now) so the guide-once,
// poll-until-up, brew-offer, and timeout semantics are covered without a real
// tailnet or stdin.
import { describe, expect, test } from "bun:test";
import { ensureTailscale } from "../src/status.ts";

function harness(states: Array<{ state: string; fqdn: string | null }>) {
  const logs: string[] = [];
  let t = 0;
  let brewCalls = 0;
  let i = 0;
  const deps = {
    resolve: () => states[Math.min(i++, states.length - 1)]!,
    log: (m: string) => { logs.push(m); },
    sleep: async (ms: number) => { t += ms; },
    now: () => t,
    brewInstall: () => { brewCalls++; },
  };
  return { deps, logs, brewCalls: () => brewCalls };
}

describe("ensureTailscale — interactive gate", () => {
  test("returns immediately when already running, no guidance", async () => {
    const h = harness([{ state: "Running", fqdn: "mac.tailXXXX.ts.net" }]);
    const r = await ensureTailscale(h.deps, 10_000, 100);
    expect(r.fqdn).toBe("mac.tailXXXX.ts.net");
    expect(h.logs.length).toBe(0);
  });

  test("guides once + polls until up, offers brew when not installed", async () => {
    const h = harness([
      { state: "NotInstalled", fqdn: null },
      { state: "NotInstalled", fqdn: null },
      { state: "Running", fqdn: "mac.tailXXXX.ts.net" },
    ]);
    const r = await ensureTailscale(h.deps, 10_000, 100);
    expect(r.fqdn).toBe("mac.tailXXXX.ts.net");
    expect(h.logs.some((l) => l.includes("brew install tailscale"))).toBe(true);
    expect(h.brewCalls()).toBe(1);
    expect(h.logs.filter((l) => l.includes("Waiting for Tailscale")).length).toBe(1);
  });

  test("throws with a re-run remedy on timeout", async () => {
    const h = harness([{ state: "Stopped", fqdn: null }]);
    await expect(ensureTailscale(h.deps, 300, 100)).rejects.toThrow(/did not come up/);
  });
});
