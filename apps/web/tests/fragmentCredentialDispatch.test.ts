// These tests cover dispatch of credentials captured before SPA startup.
// They verify success ordering and failure retention for pair credentials.
// The dispatcher is exercised without a browser runtime.

import { describe, expect, test } from "bun:test";
import type { CapturedFragmentCredential } from "../src/auth/fragment-credential.ts";
import { dispatchCapturedFragmentCredential } from "../src/store/sync-bootstrap.pair.ts";
import type { FragmentDispatcherDependencies } from "../src/store/sync-bootstrap.pair.ts";

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
      warn: (message) => events.push(`warn:${message}`),
    },
  };
}

describe("captured fragment credential dispatcher", () => {
  test("pair success clears before reload", async () => {
    const pair = dispatcherHarness({ kind: "pair", token: "pair-secret" });
    expect(await dispatchCapturedFragmentCredential(pair.deps)).toBe(true);
    expect(pair.events).toEqual([
      "pair:pair-secret",
      "clear:pair",
      "reload",
    ]);
    expect(pair.state.credential).toBeNull();
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

  test("a non-pair credential state dispatches nothing", async () => {
    const empty = dispatcherHarness({ kind: "pair", token: "unused" });
    empty.state.credential = null;
    expect(await dispatchCapturedFragmentCredential(empty.deps)).toBe(false);
    expect(empty.events).toEqual([]);
  });
});