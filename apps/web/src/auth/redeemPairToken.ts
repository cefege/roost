// Redeem a browser pairing token → authorize THIS browser with coord.
// Shared by two callers:
//   - Onboarding.tsx paste flow ("I have a pairing code")
//   - startup #pair=<token> fragment dispatcher (installer-opened browser and
//     QR-scanning phone); the dispatcher scrubs the bearer before this runs.
// On success the caller reloads so bootstrapSync re-runs with the authed JWT.

import { coordClient } from "../connect.ts";
import { getPublicKeyB64 } from "./web-key.ts";
import { trustCoordFingerprint } from "./trust.ts";
import { browserSelfLabel } from "../lib/browserSelfLabel.ts";

export type RedeemResult =
  | { ok: true }
  | { ok: false; error: string }
  // Fail-closed TOFU: coord answered with a key that doesn't match the pinned
  // fingerprint. The RPC ran, but this browser refuses to count itself as
  // redeemed until the user explicitly trusts the new key.
  | { ok: false; reason: "coord_fingerprint_mismatch" };

export async function redeemPairToken(token: string): Promise<RedeemResult> {
  try {
    const pubkeyB64 = await getPublicKeyB64();
    const result = await coordClient.authRedeemBrowser({
      token,
      sshPubkeyB64: pubkeyB64,
      label: browserSelfLabel(),
    });
    if (!result.fingerprint) return { ok: false, error: "no fingerprint in response" };
    const trusted = await trustCoordFingerprint(result.fingerprint);
    if (!trusted) return { ok: false, reason: "coord_fingerprint_mismatch" };
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
