// This module owns redemption of one-time browser pairing codes against the coordinator.
// Onboarding and the startup fragment flow call it before authenticated bootstrap reloads.
// It depends on the browser public key, the coordinator client, and the browser label.

import { Code, ConnectError } from "@connectrpc/connect";
import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";

export type RedeemResult =
  | { ok: true }
  | { ok: false; error: string; authoritative?: true };

export interface RedeemPairTokenDeps {
  getPublicKeyB64(): Promise<string>;
  browserSelfLabel(): string;
  authRedeemBrowser(input: {
    token: string;
    sshPubkeyB64: string;
    label: string;
  }): Promise<unknown>;
}

const DEFAULT_REDEEM_PAIR_TOKEN_DEPS: RedeemPairTokenDeps = {
  getPublicKeyB64,
  browserSelfLabel,
  authRedeemBrowser: (input) => coordClient.authRedeemBrowser(input),
};

export async function redeemPairToken(
  token: string,
  deps: RedeemPairTokenDeps = DEFAULT_REDEEM_PAIR_TOKEN_DEPS,
): Promise<RedeemResult> {
  try {
    const pubkeyB64 = await deps.getPublicKeyB64();
    await deps.authRedeemBrowser({
      token,
      sshPubkeyB64: pubkeyB64,
      label: deps.browserSelfLabel(),
    });
    return { ok: true };
  } catch (e) {
    const error = e instanceof Error ? e.message : String(e);
    const authoritative = e instanceof ConnectError && (
      e.code === Code.InvalidArgument
      || e.code === Code.AlreadyExists
      || e.code === Code.PermissionDenied
      || e.code === Code.Unauthenticated
    );
    return authoritative
      ? { ok: false, error, authoritative: true }
      : { ok: false, error };
  }
}
