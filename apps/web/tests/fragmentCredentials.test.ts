import { describe, expect, test } from "bun:test";
import {
  credentialFreeUrl,
  parseFragmentCredential,
} from "../src/auth/fragment-credential.ts";
import { workerCoordinatorUrl } from "../src/lib/workerCoordinatorUrl.ts";
import {
  dispatchFragmentCredential,
  type FragmentDispatcherDependencies,
} from "../src/store/sync-bootstrap.pair.ts";

describe("fragment credential classifier", () => {
  test("accepts exactly one complete pair or relocation shape", () => {
    expect(parseFragmentCredential("#pair=one-shot")).toEqual({ kind: "pair", token: "one-shot" });
    expect(parseFragmentCredential("#move=relocation&handoff=id-1")).toEqual({
      kind: "relocation",
      token: "relocation",
      handoffId: "id-1",
    });
    expect(parseFragmentCredential("#unrelated=value")).toEqual({ kind: "none" });
  });

  test("rejects combined, partial, empty, and duplicate credential fields", () => {
    for (const hash of [
      "#pair=p&move=m&handoff=h",
      "#move=m",
      "#handoff=h",
      "#move=&handoff=h",
      "#pair=",
      "#pair=a&pair=b",
      "#move=a&move=b&handoff=h",
    ]) {
      expect(parseFragmentCredential(hash), hash).toEqual({ kind: "invalid" });
    }
  });

  test("scrubs all bearer fields while preserving unrelated query and fragment entries", () => {
    expect(credentialFreeUrl(
      "/workspace",
      "?view=terminal&pair=query-is-not-a-credential",
      "#keep=one&pair=secret&other=two",
    )).toBe("/workspace?view=terminal&pair=query-is-not-a-credential#keep=one&other=two");
    expect(credentialFreeUrl("/", "?x=1", "#move=m&handoff=h")).toBe("/?x=1");
  });
});

describe("fragment credential dispatcher", () => {
  function harness(hash: string): {
    deps: FragmentDispatcherDependencies;
    events: string[];
  } {
    const events: string[] = [];
    return {
      events,
      deps: {
        pathname: "/workspace",
        search: "?view=terminal",
        hash,
        replace: (url) => events.push(`replace:${url}`),
        reload: () => events.push("reload"),
        redeemPair: async (token) => {
          events.push(`pair:${token}`);
          return { ok: true };
        },
        redeemRelocation: async (credential) => {
          events.push(`relocation:${credential.token}:${credential.handoffId}`);
          return true;
        },
        warn: (message) => events.push(`warn:${message}`),
      },
    };
  }

  test("scrubs before pair or relocation redemption and halts on success", async () => {
    const pair = harness("#keep=1&pair=secret");
    expect(await dispatchFragmentCredential(pair.deps)).toBe(true);
    expect(pair.events).toEqual([
      "replace:/workspace?view=terminal#keep=1",
      "pair:secret",
      "reload",
    ]);

    const relocation = harness("#move=move-token&handoff=id");
    expect(await dispatchFragmentCredential(relocation.deps)).toBe(true);
    expect(relocation.events).toEqual([
      "replace:/workspace?view=terminal",
      "relocation:move-token:id",
    ]);
  });

  test("failed redemption does not reload and invalid links stop before RPC", async () => {
    const failed = harness("#pair=spent");
    failed.deps.redeemPair = async (token) => {
      failed.events.push(`pair:${token}`);
      return { ok: false, error: "expired" };
    };
    expect(await dispatchFragmentCredential(failed.deps)).toBe(false);
    expect(failed.events).toEqual([
      "replace:/workspace?view=terminal",
      "pair:spent",
      "warn:[sync] #pair redeem failed: expired",
    ]);

    const invalid = harness("#pair=p&move=m&handoff=h&keep=1");
    await expect(dispatchFragmentCredential(invalid.deps)).rejects.toThrow(
      "Invalid credential link (retryable)",
    );
    expect(invalid.events).toEqual(["replace:/workspace?view=terminal#keep=1"]);
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
