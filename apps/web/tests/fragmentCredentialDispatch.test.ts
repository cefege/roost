// These tests cover dispatch of credentials captured before SPA startup.
// They verify success ordering, failure retention, and coordinator-origin validation.
// The dispatcher and URL selector are exercised without a browser runtime.

import { describe, expect, test } from "bun:test";
import type { CapturedFragmentCredential } from "../src/auth/fragment-credential.ts";
import { workerCoordinatorUrl } from "../src/lib/workerCoordinatorUrl.ts";
import { dispatchCapturedFragmentCredential } from "../src/store/sync-bootstrap.pair.ts";
import type { FragmentDispatcherDependencies } from "../src/store/sync-bootstrap.pair.ts";

const ROUTE_A = "a".repeat(64);
const ROUTE_B = "b".repeat(64);
const ACTIVATION_TOKEN = "A".repeat(43);
const RESET_TOKEN = "R".repeat(43);

interface DispatcherHarness {
  deps: FragmentDispatcherDependencies;
  events: string[];
  state: { credential: CapturedFragmentCredential | null };
}

function dispatcherHarness(credential: CapturedFragmentCredential): DispatcherHarness {
  const events: string[] = [];
  const state: { credential: CapturedFragmentCredential | null } = { credential };
  return {
    events,
    state,
    deps: {
      peek: () => state.credential,
      clear: (expectedKind) => {
        events.push(`clear:${expectedKind}`);
        if (state.credential?.kind !== expectedKind) return false;
        state.credential = null;
        return true;
      },
      reload: () => events.push("reload"),
      redeemPair: async (token) => {
        events.push(`pair:${token}`);
        return { ok: true };
      },
      redeemRelocation: async (value) => {
        events.push(`relocation:${value.token}:${value.handoffId}`);
        return "success";
      },
      warn: (message) => events.push(`warn:${message}`),
    },
  };
}

describe("captured fragment credential dispatcher", () => {
  test("pair and relocation success clear before reload", async () => {
    const pair = dispatcherHarness({ kind: "pair", token: "pair-secret" });
    expect(await dispatchCapturedFragmentCredential(pair.deps)).toBe(true);
    expect(pair.events).toEqual([
      "pair:pair-secret",
      "clear:pair",
      "reload",
    ]);
    expect(pair.state.credential).toBeNull();

    const relocation = dispatcherHarness({
      kind: "relocation",
      token: "move-secret",
      handoffId: "handoff-id",
    });
    expect(await dispatchCapturedFragmentCredential(relocation.deps)).toBe(true);
    expect(relocation.events).toEqual([
      "relocation:move-secret:handoff-id",
      "clear:relocation",
      "reload",
    ]);
    expect(relocation.state.credential).toBeNull();
  });

  test("authoritative denials clear, while ambiguous pair errors remain retryable", async () => {
    const denied = dispatcherHarness({ kind: "pair", token: "spent" });
    denied.deps.redeemPair = async (token) => {
      denied.events.push(`pair:${token}`);
      return { ok: false, error: "invalid or expired", authoritative: true };
    };
    expect(await dispatchCapturedFragmentCredential(denied.deps)).toBe(false);
    expect(denied.events).toEqual([
      "pair:spent",
      "clear:pair",
      "warn:[sync] #pair redeem failed: invalid or expired",
    ]);
    expect(denied.state.credential).toBeNull();

    const ambiguous = dispatcherHarness({ kind: "pair", token: "retry-me" });
    ambiguous.deps.redeemPair = async (token) => {
      ambiguous.events.push(`pair:${token}`);
      return { ok: false, error: "network unavailable" };
    };
    expect(await dispatchCapturedFragmentCredential(ambiguous.deps)).toBe(false);
    expect(ambiguous.events).toEqual([
      "pair:retry-me",
      "warn:[sync] #pair redeem failed: network unavailable",
    ]);
    expect(ambiguous.state.credential).toEqual({ kind: "pair", token: "retry-me" });
  });

  test("authoritative relocation denial clears, while retryable failure remains captured", async () => {
    const denied = dispatcherHarness({
      kind: "relocation",
      token: "denied",
      handoffId: "handoff",
    });
    denied.deps.redeemRelocation = async () => "authoritative-denial";
    expect(await dispatchCapturedFragmentCredential(denied.deps)).toBe(false);
    expect(denied.events).toEqual([
      "clear:relocation",
      "warn:[sync] coordinator relocation credential was denied",
    ]);
    expect(denied.state.credential).toBeNull();

    const retryable = dispatcherHarness({
      kind: "relocation",
      token: "retry",
      handoffId: "handoff",
    });
    retryable.deps.redeemRelocation = async () => "retryable";
    expect(await dispatchCapturedFragmentCredential(retryable.deps)).toBe(false);
    expect(retryable.events).toEqual([]);
    expect(retryable.state.credential).toEqual({
      kind: "relocation",
      token: "retry",
      handoffId: "handoff",
    });
  });

  test("activation and reset credentials are left for their route forms", async () => {
    for (const credential of [
      { kind: "activation", token: ACTIVATION_TOKEN, routeKey: ROUTE_A },
      { kind: "reset", token: RESET_TOKEN, routeKey: ROUTE_B },
    ] as const) {
      const harness = dispatcherHarness(credential);
      expect(await dispatchCapturedFragmentCredential(harness.deps)).toBe(false);
      expect(harness.events).toEqual([]);
      expect(harness.state.credential).toEqual(credential);
    }
  });
});

describe("worker coordinator origin", () => {
  test("requires a distinct non-loopback HTTPS origin", () => {
    expect(workerCoordinatorUrl("https://private.example.ts.net:4102", "https://roost.example.com"))
      .toBe("https://private.example.ts.net:4102");
    for (const configured of [
      undefined,
      "http://private.example.ts.net:4102",
      "https://localhost:4102",
      "https://127.0.0.1:4102",
      "https://127.0.0.2:4102",
      "https://roost.example.com",
      "https://roost.example.com/",
      "https://private.example.ts.net:4102/?token=bad",
      "https://roost.example.com/path",
    ]) {
      expect(workerCoordinatorUrl(configured, "https://roost.example.com")).toBeNull();
    }
  });
});