import { Code, ConnectError } from "@connectrpc/connect";
import { describe, expect, test } from "bun:test";

interface RedeemInput {
  token: string;
  sshPubkeyB64: string;
  label: string;
}

const redeemCalls: RedeemInput[] = [];
let nextFailure: unknown | null = null;


// Load the real module; explicit dependencies keep this test hermetic without
// process-wide module mocks that can poison the web-key migration suite.
const { redeemPairToken } = await import("../src/auth/redeemPairToken.ts");
const redeemDeps = {
  getPublicKeyB64: async () => "dGVzdC1wdWJsaWMta2V5",
  browserSelfLabel: () => "Test browser",
  authRedeemBrowser: async (input: RedeemInput) => {
    redeemCalls.push(input);
    if (nextFailure !== null) throw nextFailure;
    return {};
  },
};

function resetRedemption(): void {
  redeemCalls.length = 0;
  nextFailure = null;
}

describe("redeemPairToken", () => {
  test("a successful HTTPS redemption needs no coordinator key assertion", async () => {
    resetRedemption();

    expect(await redeemPairToken("tok-success", redeemDeps)).toEqual({ ok: true });
    expect(redeemCalls).toEqual([{
      token: "tok-success",
      sshPubkeyB64: "dGVzdC1wdWJsaWMta2V5",
      label: "Test browser",
    }]);
  });

  test("credential and request denials remain authoritative", async () => {
    for (const code of [
      Code.InvalidArgument,
      Code.AlreadyExists,
      Code.PermissionDenied,
      Code.Unauthenticated,
    ]) {
      resetRedemption();
      const failure = new ConnectError("redemption denied", code);
      nextFailure = failure;

      expect(await redeemPairToken(`tok-${code}`, redeemDeps)).toEqual({
        ok: false,
        error: failure.message,
        authoritative: true,
      });
    }
  });

  test("transport and unknown failures remain retryable", async () => {
    resetRedemption();
    const unavailable = new ConnectError("coordinator unavailable", Code.Unavailable);
    nextFailure = unavailable;
    expect(await redeemPairToken("tok-unavailable", redeemDeps)).toEqual({
      ok: false,
      error: unavailable.message,
    });

    resetRedemption();
    const network = new TypeError("network failed");
    nextFailure = network;
    expect(await redeemPairToken("tok-network", redeemDeps)).toEqual({
      ok: false,
      error: network.message,
    });
  });
});
